//! The WebKit window: what the engine's news and the toolbar do to the chrome.
//!
//! The engine prints the page markers (`WEBKIT:W5:LOAD|TITLE|...`, they are
//! WebKit's own word); this side prints `WEBKIT:W5:UI:*` for the window.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use xui_core::app::{App, Ui};
use xui_core::backend::{Result, WidgetId};
use xui_core::geometry::Rect;
use xui_core::icon::Lucide;
use xui_core::widget::{Button, Edit, HasText, Label, Placeable};
use xui_core::{Key, Modifiers};

use crate::address;
use crate::chrome::{self, Widgets};
use crate::engine::Engine;
use crate::mailbox::{Mailbox, Notice, PageState};
use crate::view::PageView;

/// The window's name while the page has no title.
pub const APP_NAME: &str = "WebKit";

/// Everything the window reacts to.
#[derive(Clone, Debug)]
pub enum Msg {
    /// The engine painted a frame.
    Frame,
    /// The engine's page state changed.
    State,
    Back,
    Forward,
    /// The toolbar's Reload button, which is Stop while a page loads.
    ReloadOrStop,
    /// Enter in the address field, or the Go button.
    Go,
    /// Ctrl+L.
    FocusAddress,
    /// Escape in the address field.
    RestoreAddress,
    /// The user changed the address field's text.
    AddressEdited,
    /// The user clicked in the page: whatever was half-typed in the address field is abandoned.
    PageClicked,
}

/// Tells a session script where a named control is: (window title, name, rectangle in the
/// window's content, physical pixels).
pub type Probe = Box<dyn Fn(&str, &str, Rect)>;

/// What the platform adds to the window.
pub struct Host {
    /// Print `WEBKIT:W5:UI:NAV:<url>` for each address opened: the UI probe's, so a shipped
    /// browser does not log what the user types.
    pub traces: bool,
    /// The widget holding the keyboard focus (Enter and Escape belong to the address field
    /// only while it has it).
    pub focused: Box<dyn Fn() -> Option<WidgetId>>,
    /// The probe (a normal image has none: nothing is printed).
    pub probe: Probe,
}

impl Host {
    /// A host with nothing to add.
    pub fn none() -> Host {
        Host {
            traces: false,
            focused: Box::new(|| None),
            probe: Box::new(|_, _, _| {}),
        }
    }
}

pub struct Browser {
    page: Rc<PageView>,
    address: Rc<Edit<Msg>>,
    back: Rc<Button<Msg>>,
    forward: Rc<Button<Msg>>,
    reload: Rc<Button<Msg>>,
    status: Rc<Label<Msg>>,
    engine: Rc<dyn Engine>,
    mailbox: Arc<Mailbox>,
    probe: Probe,
    traces: bool,
    /// The page as the engine last reported it.
    state: PageState,
    /// The window's caption as last set.
    title: String,
    /// The page view's size when the controls were last announced.
    announced: Cell<(i32, i32)>,
    /// The user is typing an address: the page's own news must not replace it.
    editing: bool,
    /// The previous picture's buffer, reused for the next frame.
    spare: Vec<u8>,
    /// Back, Forward and loading as last shown, so a button is touched only
    /// when it changed (setting one repaints it).
    buttons: Cell<Option<[bool; 3]>>,
    frames: u64,
}

impl Browser {
    /// Builds the window; `first` is loaded at once.
    pub fn build(
        ui: &mut Ui<Msg>,
        engine: Rc<dyn Engine>,
        mailbox: Arc<Mailbox>,
        first: Option<String>,
        host: Host,
    ) -> Result<Browser> {
        let widgets = Widgets::default();
        ui.root(chrome::window(&widgets, Rc::clone(&engine)))?;

        let proxy = ui.proxy();
        mailbox.set_notify(move |notice| {
            let _ = proxy.send(match notice {
                Notice::Frame => Msg::Frame,
                Notice::State => Msg::State,
            });
        });

        let address = widgets.address.get();
        let field = address.id();
        let Host {
            traces,
            focused,
            probe,
        } = host;
        ui.on_key(move |key, mods| shortcut(key, mods, focused() == Some(field)));
        ui.set_window_title(APP_NAME);

        let browser = Browser {
            page: widgets.page.get(),
            address,
            back: widgets.back.get(),
            forward: widgets.forward.get(),
            reload: widgets.reload.get(),
            status: widgets.status.get(),
            engine,
            mailbox,
            probe,
            traces,
            state: PageState::default(),
            title: APP_NAME.to_owned(),
            announced: Cell::new((0, 0)),
            editing: false,
            spare: Vec::new(),
            buttons: Cell::new(None),
            frames: 0,
        };
        browser.update_buttons();
        if let Some(url) = first {
            browser.address.set_text(&url);
            browser.trace_navigation(&url);
            browser.engine.load(&url);
        }
        browser.page.focus();
        Ok(browser)
    }

    /// Frames shown so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The page as the engine last reported it.
    pub fn page_state(&self) -> &PageState {
        &self.state
    }

    /// The page view, for a test to look at.
    pub fn page(&self) -> &Rc<PageView> {
        &self.page
    }

    fn trace_navigation(&self, url: &str) {
        if self.traces {
            println!("WEBKIT:W5:UI:NAV:{url}");
        }
    }

    /// Opens what the address field holds.
    fn go(&mut self) {
        let text = self.address.text();
        match address::normalize(&text) {
            Some(url) => {
                self.address.set_text(&url);
                self.editing = false;
                self.trace_navigation(&url);
                self.engine.load(&url);
                self.page.focus();
            }
            None => self.status.set_text("Type an address first"),
        }
    }

    fn show_frame(&mut self, ui: &Ui<Msg>) {
        if let Some(frame) = self.mailbox.take_frame(std::mem::take(&mut self.spare)) {
            if self.state.crashed {
                // A picture painted just before the page's process went away.
                self.spare = frame.rgba;
                return;
            }
            self.spare = self.page.show(frame);
            self.frames += 1;
            if self.frames == 1 {
                let (width, height) = frame_size(&self.page);
                println!("WEBKIT:W5:UI:FRAME:{width}x{height}");
            }
            // The first frame, and the first after the window changed size: where the controls are.
            let page = ui.bounds(self.page.id());
            if self.announced.get() != (page.width(), page.height()) {
                self.announce(ui);
            }
        }
    }

    /// Says where the controls are, for a session (`xui_app::probe`): the page as a whole and
    /// its top-left corner as a one-pixel control, so a script can click page coordinates.
    fn announce(&self, ui: &Ui<Msg>) {
        let page = ui.bounds(self.page.id());
        self.announced.set((page.width(), page.height()));
        let named = [
            ("address", ui.bounds(self.address.id())),
            ("back", ui.bounds(self.back.id())),
            ("forward", ui.bounds(self.forward.id())),
            ("reload", ui.bounds(self.reload.id())),
            ("page", page),
            (
                "page_origin",
                Rect::new(page.left, page.top, page.left + 1, page.top + 1),
            ),
        ];
        for (name, rect) in named {
            (self.probe)(&self.title, name, rect);
        }
    }

    fn show_state(&mut self, ui: &Ui<Msg>) {
        let Some(state) = self.mailbox.take_state() else {
            return;
        };
        if !self.editing && state.url != self.state.url {
            self.address.set_text(&state.url);
        }
        let caption = caption(&state.title);
        let retitled = caption != self.title;
        if retitled {
            ui.set_window_title(&caption);
            self.title = caption;
        }
        let text = status_text(&state);
        if state.crashed {
            self.page.clear();
        }
        if self.traces && !state.error.is_empty() && state.error != self.state.error {
            println!("WEBKIT:W5:UI:ERROR:{}", state.error);
        }
        self.state = state;
        self.status.set_text(&text);
        self.update_buttons();
        if retitled {
            self.announce(ui);
        }
    }

    /// Enables Back and Forward, and turns Reload into Stop while a page loads.
    fn update_buttons(&self) {
        let shown = [
            self.state.can_back,
            self.state.can_forward,
            self.state.loading,
        ];
        if self.buttons.replace(Some(shown)) == Some(shown) {
            return;
        }
        self.back.set_enabled(shown[0]);
        self.forward.set_enabled(shown[1]);
        let (icon, tip) = if shown[2] {
            // F5 and Ctrl+R stop a load too (Esc belongs to the page's own forms).
            (Lucide::X, "Stop (F5)")
        } else {
            (Lucide::RefreshCw, "Reload (F5)")
        };
        self.reload.set_icon(Some(icon));
        let _ = self.reload.set_tooltip(tip);
    }
}

/// The window's caption for a page title.
fn caption(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        APP_NAME.to_owned()
    } else {
        format!("{title} - {APP_NAME}")
    }
}

/// The size of the picture on show (0 x 0 before the first).
fn frame_size(page: &PageView) -> (u32, u32) {
    page.picture_size().unwrap_or((0, 0))
}

/// The status bar's text.
fn status_text(state: &PageState) -> String {
    if !state.error.is_empty() && !state.loading {
        // A failure says so (and what failed: the page on show is the old one) and stays up
        // until the next load: "Done" would be a lie.
        state.error.clone()
    } else if state.loading {
        format!("Loading {}  {}%", state.url, state.progress / 10)
    } else if state.url.is_empty() {
        String::new()
    } else {
        format!("Done  {}", state.url)
    }
}

/// The window's keyboard shortcuts; Enter and Escape mean the address field
/// only while it has the focus, so the page's own forms still get them.
pub fn shortcut(key: Key, mods: Modifiers, in_address: bool) -> Option<Msg> {
    match key {
        Key::RETURN if in_address => Some(Msg::Go),
        Key::ESCAPE if in_address => Some(Msg::RestoreAddress),
        Key::LEFT if mods.alt => Some(Msg::Back),
        Key::RIGHT if mods.alt => Some(Msg::Forward),
        Key::F5 => Some(Msg::ReloadOrStop),
        Key::R if mods.ctrl => Some(Msg::ReloadOrStop),
        Key::L if mods.ctrl => Some(Msg::FocusAddress),
        _ => None,
    }
}

impl App for Browser {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Frame => self.show_frame(ui),
            Msg::State => self.show_state(ui),
            Msg::Back => self.engine.back(),
            Msg::Forward => self.engine.forward(),
            Msg::ReloadOrStop => {
                if self.state.loading {
                    self.engine.stop();
                } else {
                    self.engine.reload();
                }
            }
            Msg::Go => self.go(),
            Msg::FocusAddress => {
                // Cleared, so typing replaces the address as it would if it were selected: the
                // pinned xui's Edit has no select-all call (Ctrl+A and a triple click do it).
                // Escape brings the old address back.
                self.address.set_text("");
                self.address.focus();
                self.editing = true;
            }
            Msg::RestoreAddress => {
                self.editing = false;
                let url = self.state.url.clone();
                self.address.set_text(&url);
                self.page.focus();
            }
            Msg::AddressEdited => self.editing = true,
            Msg::PageClicked => {
                if self.editing {
                    // The page has the keyboard again: its address is the field's again.
                    self.editing = false;
                    let url = self.state.url.clone();
                    self.address.set_text(&url);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_and_escape_belong_to_the_page_unless_the_address_has_the_keyboard() {
        let none = Modifiers::NONE;
        assert!(matches!(shortcut(Key::RETURN, none, true), Some(Msg::Go)));
        assert!(shortcut(Key::RETURN, none, false).is_none());
        assert!(matches!(
            shortcut(Key::ESCAPE, none, true),
            Some(Msg::RestoreAddress)
        ));
        assert!(shortcut(Key::ESCAPE, none, false).is_none());
    }

    #[test]
    fn navigation_chords() {
        let alt = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        assert!(matches!(shortcut(Key::LEFT, alt, false), Some(Msg::Back)));
        assert!(matches!(
            shortcut(Key::RIGHT, alt, false),
            Some(Msg::Forward)
        ));
        assert!(matches!(
            shortcut(Key::L, ctrl, false),
            Some(Msg::FocusAddress)
        ));
        assert!(matches!(
            shortcut(Key::F5, Modifiers::NONE, false),
            Some(Msg::ReloadOrStop)
        ));
        assert!(shortcut(Key::LEFT, Modifiers::NONE, false).is_none());
    }

    #[test]
    fn the_caption_names_the_page_then_the_app() {
        assert_eq!(caption("Example Domain"), "Example Domain - WebKit");
        assert_eq!(caption("  "), "WebKit");
        assert_eq!(caption(""), "WebKit");
    }

    #[test]
    fn the_status_line_follows_the_load() {
        let loading = PageState {
            url: "http://a/".into(),
            loading: true,
            progress: 420,
            ..PageState::default()
        };
        assert_eq!(status_text(&loading), "Loading http://a/  42%");
        let done = PageState {
            loading: false,
            ..loading
        };
        assert_eq!(status_text(&done), "Done  http://a/");
        assert_eq!(status_text(&PageState::default()), "");
    }

    #[test]
    fn a_failed_load_is_not_reported_as_done() {
        let failed = PageState {
            url: "http://a:81/".into(),
            error: "Could not load http://a:81/: Connection refused".into(),
            ..PageState::default()
        };
        assert_eq!(
            status_text(&failed),
            "Could not load http://a:81/: Connection refused"
        );
        // The next load starts clean: the engine clears the error, and a load in progress says so.
        let retry = PageState {
            loading: true,
            progress: 100,
            ..failed
        };
        assert!(status_text(&retry).starts_with("Loading http://a:81/"));
    }
}
