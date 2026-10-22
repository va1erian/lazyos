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
use xui_core::backend::{Result, TimerId};
use xui_core::widget::{Button, DialogAction, Edit, HasText, Label, Menu, Placeable, ProgressBar};
use xui_webkit::engine::Engine;
use xui_webkit::mailbox::{Mailbox, Notice, PageState};
use xui_webkit::view::PageView;

mod commands;
mod events;
mod frame;
mod hang;
mod host;
mod notifications;
mod search;
mod shield;
mod status;
mod suggest;
mod webd;

pub use host::{Host, Probe, Setup};

use crate::address;
use crate::blocker::{self, Blocker};
use crate::chrome::{self, Command, Widgets};
use crate::context::Context;
use crate::dialogs::Dialogs;
use crate::downgrade::{self, Allowed, Pending};
use crate::frameclock::FrameClock;
use crate::hang::Answer as HangAnswer;
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
    /// The person acted on the suggestions under the address field.
    Suggest(crate::suggest::dropdown::Act),
    /// The user clicked in the page: whatever was half-typed in the address field is abandoned.
    PageClicked,
    /// A right click on the page, at a point of the view.
    PageMenu(i32, i32),
    /// The answer to a JavaScript dialog.
    Dialog(DialogAction),
    /// The person answered the bar a site's request to notify them shows.
    NotifyAnswer(BarAnswer),
    /// The person answered the bar a page that does not respond shows.
    Hang(HangAnswer),
    /// The shell's worker has news about the toasts (a toast is up, clicked, closed).
    Shell,
    /// A list of the ad blocker changed state on the worker's thread.
    BlockerChanged,
    /// webd's view has news, or a call waits its turn (`Host::pump`).
    Engine,
    /// Other threads queued work for the window's thread (`Host::jobs`).
    Jobs,
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
    /// The "not responding" bar and its question.
    hang: hang::Bar,
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
    /// What the shield shows now (blocked, applies, ready): touched only on a change.
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
    /// The history suggestions under the address field.
    suggest: suggest::Panel,
    /// The title the visit was last recorded with (a title can arrive after the load).
    recorded: Option<(String, String)>,
    /// A host typed in the address bar and opened over HTTPS, not yet known to have worked.
    pending_https: Option<Pending>,
    /// The host the "did not answer over HTTPS" page is asking about.
    offered: Option<Pending>,
    /// The hosts the person loaded without encryption in this window's life.
    allowed_http: Allowed,
    /// The page view's size when the controls were last announced.
    announced: Cell<(i32, i32)>,
    /// The previous picture's buffer, reused for the next frame.
    spare: Vec<u8>,
    /// Back, Forward and loading as last shown (a button repaints when set).
    buttons: Cell<Option<[bool; 3]>>,
    frames: u64,
    /// What the window thread spends on the page, printed once a second (`INFO:WINDOW:`).
    clock: FrameClock,
    /// The short timer that runs while the engine wants to be called soon (`app/webd.rs`).
    fast: Cell<Option<TimerId>>,
    /// The status bar's shown text (for the tests).
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
        let suggestions_open = Rc::new(Cell::new(false));
        let key_suggestions = Rc::clone(&suggestions_open);
        ui.on_key(move |key, mods| {
            let in_address = focused() == Some(field);
            // While the dropdown has rows, its keys come before the window's (Escape puts it away
            // before it restores the address).
            if in_address && key_suggestions.get() {
                if let Some(msg) = suggest::key(key, mods) {
                    return Some(msg);
                }
            }
            shortcut(key, mods, in_address, key_loading.get())
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
            hang: hang::Bar::new(&widgets),
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
            suggest: suggest::Panel::new(&widgets, suggestions_open),
            recorded: None,
            pending_https: None,
            offered: None,
            allowed_http: Allowed::default(),
            announced: Cell::new((0, 0)),
            spare: Vec::new(),
            buttons: Cell::new(None),
            frames: 0,
            clock: FrameClock::default(),
            fast: Cell::new(None),
            shown: RefCell::new(String::new()),
        };
        browser.start_engine(ui);
        let first = url.unwrap_or_else(|| browser.internal.start());
        browser.start_navigation(&first);
        browser.update_buttons();
        browser.update_downloads(ui);
        browser.bar.start_hidden(ui);
        browser.hang.start_hidden(ui);
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
            downgrade::NAME => self.downgrade_page(),
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
        match address::classify(&text, &self.host.prefs.search) {
            Some(typed) => self.open_typed(typed),
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
        // Enter opens the highlighted suggestion, if the person moved onto one, before the
        // dropdown goes.
        let chosen = self.suggest.chosen().filter(|_| matches!(msg, Msg::Go));
        if suggest::closes(&msg) {
            self.hide_suggest(ui);
        }
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
            Msg::Go => match chosen {
                Some(row) => self.open_row(row),
                None => self.go(),
            },
            Msg::FocusAddress => self.focus_address(),
            Msg::RestoreAddress => {
                self.editing = false;
                let shown = self.shown_url(&self.state.url);
                self.address.set_text(&shown);
                self.page.focus();
            }
            Msg::AddressEdited => {
                self.editing = true;
                self.address_edited(ui);
            }
            Msg::Suggest(act) => self.suggest_act(ui, act),
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
            Msg::Hang(answer) => self.hang_answer(ui, answer),
            Msg::Shell => self.shell_news(ui),
            Msg::Engine => self.pump(ui),
            Msg::Jobs => (self.host.jobs)(),
        }
        self.update_buttons();
    }
}
