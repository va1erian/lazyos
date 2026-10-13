//! The LazyWebPlus window: what the engine's news, the menus, the toolbar and the keyboard do to
//! the chrome (`chrome.rs`).
//!
//! Serial evidence (the screenshot sessions wait on it; the markers that name a page are printed
//! only in a UI-probe image, `LAZYOS_UI_PROBE=1`, so a shipped browser does not log what its user
//! visits): `WEBPLUS:UP:PASS` after the first frame, `WEBPLUS:NAV:<url>` when the window starts
//! a navigation, `WEBPLUS:LOAD:<url>` then `WEBPLUS:TITLE:<title>` when a load finishes,
//! `WEBPLUS:FAIL:<reason>` when one fails, `WEBPLUS:LAUNCH:...` (`handoff.rs`) when a link goes
//! to another app, `WEBPLUS:POPUP:<url>` for a new-window request, `WEBPLUS:DIALOG:<kind>` for
//! a JavaScript dialog, `WEBPLUS:CURSOR:<kind>` when the page changes the pointer, and the
//! download markers of `transfers.rs`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use xui_core::app::{App, Ui};
use xui_core::backend::Result;
use xui_core::geometry::Rect;
use xui_core::icon::Lucide;
use xui_core::widget::{Button, DialogAction, Edit, HasText, Label, Menu, Placeable, ProgressBar};
use xui_webkit::engine::Engine;
use xui_webkit::mailbox::{Mailbox, Notice, PageState};
use xui_webkit::view::PageView;

mod commands;
mod events;
mod host;
mod notifications;

pub use host::{Host, Probe, Setup};

use crate::address;
use crate::blocker::{self, Blocker, ListState};
use crate::chrome::{self, Command, Widgets};
use crate::context::Context;
use crate::dialogs::Dialogs;
use crate::indicators::{Badge, Throbber};
use crate::internal::Internal;
use crate::keys::shortcut;
use crate::marker_text;
use crate::notify::{BarAnswer, Notifications};
use crate::pages::{self, ABOUT, BLOCKER, DOWNLOADS, HISTORY};
use crate::transfers::Transfers;
use crate::visits::Visits;

/// The name the window has while a page has no title.
pub const APP_NAME: &str = "LazyWeb+";

/// Everything the window reacts to.
#[derive(Clone)]
pub enum Msg {
    /// The engine painted a frame.
    Frame,
    /// The engine's page state changed.
    State,
    /// The engine has news (a download, a dialog, the link under the pointer, ...).
    Events,
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
    /// The user clicked in the page: whatever was half-typed in the address field is abandoned.
    PageClicked,
    /// A right click on the page, at a point of the view.
    PageMenu(i32, i32),
    /// The answer to a JavaScript dialog.
    Dialog(DialogAction),
    /// The person answered the bar a site's request to notify them shows.
    NotifyAnswer(BarAnswer),
    /// The shell's worker has news about the toasts (a toast is up, clicked, closed).
    Shell,
    /// A list of the ad blocker changed state on the worker's thread.
    BlockerChanged,
}

/// The window's widgets and state.
pub struct Browser {
    page: Rc<PageView<Msg>>,
    menu: Rc<Menu<Msg>>,
    context: Context,
    address: Rc<Edit<Msg>>,
    back: Rc<Button<Msg>>,
    forward: Rc<Button<Msg>>,
    reload: Rc<Button<Msg>>,
    throbber: Rc<Throbber>,
    shield: Rc<Button<Msg>>,
    status: Rc<Label<Msg>>,
    badge: Rc<Badge>,
    download_label: Rc<Label<Msg>>,
    download_bar: Rc<ProgressBar<Msg>>,
    dialogs: Dialogs,
    /// The permission bar and what it is asking about.
    bar: notifications::Bar,
    notifications: Notifications,
    /// When the window was built: the clock the notification limits run on.
    started: Instant,
    engine: Rc<dyn Engine>,
    mailbox: Arc<Mailbox>,
    visits: Visits,
    transfers: Transfers,
    internal: Internal,
    blocker: Blocker,
    /// The web page browsed last (not one of our own): what the ad blocker's "this site" means.
    site_url: Option<String>,
    /// What the shield shows now: (blocked, applies, ready), so it is touched only on a change.
    shield_shown: Cell<Option<(u32, bool, bool)>>,
    host: Host,
    /// The page as the engine last reported it.
    state: PageState,
    /// The window's caption as last set.
    caption: String,
    /// What the status bar says when no link is under the pointer.
    message: String,
    /// The link under the pointer.
    hover: String,
    /// A load is under way (shared with the page view: Escape means Stop only then).
    loading: Rc<Cell<bool>>,
    /// The user pressed Stop: the end of the load that follows is a stop, not a failure.
    stopping: bool,
    /// The user is typing an address: the page's own news (its URL as it loads, redirects) must
    /// not replace what they typed.
    editing: bool,
    /// The title the visit was last recorded with (a title can arrive after the load).
    recorded: Option<(String, String)>,
    /// The page view's size when the controls were last announced.
    announced: Cell<(i32, i32)>,
    /// The previous picture's buffer, reused for the next frame.
    spare: Vec<u8>,
    /// Back, Forward and loading as last shown: setting a button repaints it, and state arrives
    /// many times a second.
    buttons: Cell<Option<[bool; 3]>>,
    frames: u64,
    /// Whether the status bar's shown text is the one in `message`/`hover`, for the tests.
    shown: RefCell<String>,
}

impl Browser {
    /// Builds the window, opening `setup.url` (or the start page).
    pub fn build(
        ui: &mut Ui<Msg>,
        engine: Rc<dyn Engine>,
        mailbox: Arc<Mailbox>,
        setup: Setup,
    ) -> Result<Browser> {
        let Setup {
            url,
            visits,
            transfers,
            internal,
            permissions,
            toasts,
            blocker,
            host,
        } = setup;
        let loading = Rc::new(Cell::new(false));
        let widgets = Widgets::default();
        ui.root(chrome::window(
            &widgets,
            Rc::clone(&engine),
            Rc::clone(&loading),
        ))?;

        let proxy = ui.proxy();
        let blocker_proxy = ui.proxy();
        blocker.on_change(move || {
            let _ = blocker_proxy.send(Msg::BlockerChanged);
        });
        mailbox.set_notify(move |notice| {
            let _ = proxy.send(match notice {
                Notice::Frame => Msg::Frame,
                Notice::State => Msg::State,
                Notice::Events => Msg::Events,
            });
        });

        let address = widgets.address.get();
        let field = address.id();
        let key_loading = Rc::clone(&loading);
        let focused = Rc::clone(&host.focused);
        ui.on_key(move |key, mods| {
            shortcut(key, mods, focused() == Some(field), key_loading.get())
        });

        let context = Context::new(ui);
        let mut browser = Browser {
            page: widgets.page.get(),
            menu: widgets.menu.get(),
            context,
            address,
            back: widgets.back.get(),
            forward: widgets.forward.get(),
            reload: widgets.reload.get(),
            throbber: widgets.throbber.get(),
            shield: widgets.shield.get(),
            status: widgets.status.get(),
            badge: widgets.badge.get(),
            download_label: widgets.download_label.get(),
            download_bar: widgets.download_bar.get(),
            dialogs: Dialogs::new(ui)?,
            bar: notifications::Bar::new(&widgets),
            notifications: Notifications::new(permissions, toasts),
            started: Instant::now(),
            engine,
            mailbox,
            visits,
            transfers,
            internal,
            blocker,
            site_url: None,
            shield_shown: Cell::new(None),
            host,
            state: PageState::default(),
            caption: APP_NAME.to_owned(),
            message: String::new(),
            hover: String::new(),
            loading,
            stopping: false,
            editing: false,
            recorded: None,
            announced: Cell::new((0, 0)),
            spare: Vec::new(),
            buttons: Cell::new(None),
            frames: 0,
            shown: RefCell::new(String::new()),
        };
        let first = url.unwrap_or_else(|| browser.internal.start());
        browser.start_navigation(&first);
        browser.update_buttons();
        browser.update_downloads(ui);
        browser.bar.start_hidden(ui);
        browser.update_shield();
        ui.set_window_title(APP_NAME);
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
    pub fn page(&self) -> &Rc<PageView<Msg>> {
        &self.page
    }

    /// What the status bar shows.
    pub fn status_text(&self) -> String {
        self.shown.borrow().clone()
    }

    fn trace(&self, line: &str) {
        if self.host.traces {
            println!("WEBPLUS:{line}");
        }
    }

    fn set_loading(&mut self, on: bool) {
        self.loading.set(on);
        self.throbber.set_spinning(on);
    }

    /// Shows `text` in the status bar until something replaces it; a link under the pointer is
    /// shown over it.
    fn set_status(&mut self, text: &str) {
        self.message = text.to_owned();
        self.refresh_status();
    }

    fn refresh_status(&self) {
        let text = if self.hover.is_empty() {
            &self.message
        } else {
            &self.hover
        };
        if *self.shown.borrow() != *text {
            self.status.set_text(text);
            self.shown.replace(text.clone());
        }
    }

    /// The address the bar shows for the engine's `url`: our pages by name.
    fn shown_url(&self, url: &str) -> String {
        self.internal.shown(url).to_owned()
    }

    /// Enables Back, Forward and Stop, and turns Reload into Stop while a page
    /// loads, touching them only when that changed.
    fn update_buttons(&self) {
        let state = [
            self.state.can_back,
            self.state.can_forward,
            self.state.loading,
        ];
        if self.buttons.replace(Some(state)) == Some(state) {
            return;
        }
        self.back.set_enabled(state[0]);
        self.forward.set_enabled(state[1]);
        let (icon, tip) = if state[2] {
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

    /// Shows what the ad blocker does for the page on show: the icon is the eye that sees nothing
    /// while it blocks, and the tooltip says how much it stopped.
    pub(super) fn update_shield(&self) {
        let site = self.site_url.as_deref().and_then(blocker::site_of);
        let enabled = self.blocker.settings().enabled;
        let excepted = site
            .as_deref()
            .is_some_and(|site| self.blocker.site_excepted(site));
        let applies = enabled && !excepted;
        let ready = self.blocker.any_ready();
        let blocked = self.blocker.blocked();
        let key = (blocked, applies, ready);
        if self.shield_shown.replace(Some(key)) == Some(key) {
            return;
        }
        let (icon, tip) = if !enabled {
            (Lucide::Eye, String::from("Ad blocker: off"))
        } else if excepted {
            let site = site.unwrap_or_default();
            (Lucide::Eye, format!("Ad blocker: off for {site}"))
        } else if !ready {
            let lists = self.blocker.lists();
            let why = match lists.iter().find(|l| l.enabled).map(|l| &l.state) {
                None => "no list is on",
                Some(ListState::Failed(_)) => "a list does not work",
                Some(ListState::Unavailable) => "not available in this build",
                Some(_) => "preparing its lists",
            };
            (Lucide::EyeOff, format!("Ad blocker: {why}"))
        } else if blocked == 0 {
            (
                Lucide::EyeOff,
                String::from("Ad blocker: on, nothing blocked here"),
            )
        } else {
            (
                Lucide::EyeOff,
                format!("Ad blocker: {blocked} blocked on this page"),
            )
        };
        self.shield.set_icon(Some(icon));
        let _ = self.shield.set_tooltip(&tip);
    }

    /// Builds one of the browser's own pages as it stands now.
    fn build_page(&mut self, name: &'static str) -> String {
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
            _ => return self.internal.start(),
        };
        self.internal.build(name, &html)
    }

    /// Opens `target`: a URL, or the name of one of the browser's own pages. A scheme the engine
    /// does not load (typed in the address bar: the user's own act) goes to the app that has it.
    fn open(&mut self, target: &str) {
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
    fn start_navigation(&mut self, url: &str) {
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
                self.page.focus();
            }
            None => self.set_status("Type an address first"),
        }
    }

    /// The page on show is one of the browser's own, named `name`.
    fn showing(&self, name: &str) -> bool {
        let url = if self.state.committed_url.is_empty() {
            &self.state.url
        } else {
            &self.state.committed_url
        };
        self.internal.name_for(url) == Some(name)
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
        let mut named = vec![
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
        // The bar's own controls, while it is up: a session answers it by name.
        if self.bar.shown().is_some() {
            named.extend(self.bar.controls(ui));
        }
        for (name, rect) in named {
            (self.host.probe)(&self.caption, name, rect);
        }
    }
}

/// The window's caption for a page title.
pub fn caption(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        APP_NAME.to_owned()
    } else {
        format!("{title} - {APP_NAME}")
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
            Msg::Frame => self.show_frame(ui),
            Msg::State => self.show_state(ui),
            Msg::Events => self.show_events(ui),
            Msg::Menu(command) => self.command(command, ui),
            Msg::ReloadOrStop => {
                let command = if self.state.loading {
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
                let shown = self.shown_url(&self.state.url);
                self.address.set_text(&shown);
                self.page.focus();
            }
            Msg::AddressEdited => self.editing = true,
            Msg::PageClicked => {
                if self.editing {
                    // The page has the keyboard again: its address is the field's again.
                    self.editing = false;
                    let shown = self.shown_url(&self.state.url);
                    self.address.set_text(&shown);
                }
            }
            Msg::PageMenu(x, y) => self.show_context(ui, (x, y)),
            Msg::BlockerChanged => self.blocker_changed(),
            Msg::Dialog(action) => {
                if let Some(answer) = self.dialogs.answer(action) {
                    self.engine
                        .dialog_reply(answer.id, answer.accepted, &answer.value);
                }
            }
            Msg::NotifyAnswer(answer) => self.notify_answer(ui, answer),
            Msg::Shell => self.shell_news(ui),
        }
        self.update_buttons();
    }
}
