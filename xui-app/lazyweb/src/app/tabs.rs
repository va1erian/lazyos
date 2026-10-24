//! The window's tabs (docs/lazyweb-plan.md 2.1-2.3): one engine, a view of webd, per tab; the page
//! view and the mailbox are pointed at the front tab's, and a tab that goes behind another gives its
//! slot memory back (`Engine::set_visible`). `crate::tabs` decides (order, the front tab, what to
//! discard); this is what the window does about it. The news of tabs behind the front one is
//! `tabnews.rs`.
//!
//! Serial evidence: `LAZYWEB:TAB:NEW`, `TAB:SWITCH`, `TAB:CLOSE`, `TAB:REOPEN`, `TAB:DISCARD`, each
//! with the number of tabs and never an address.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use xui_core::app::Ui;
use xui_core::widget::Placeable;
use xui_webkit::mailbox::{Mailbox, Notice, PageState};

use super::{Browser, Msg};
use crate::chrome::Widgets;
use crate::session;
use crate::strip::{TabMsg, TabStrip};
use crate::switch::Switch;
use crate::tabs::{process_key, Place, Raised, TabId, Tabs};

/// The most tabs that hold a view at once; the oldest used behind the front one give theirs up
/// past it. Provisional: the plan picks the number from the memory of real pages (2.5).
pub(super) const LIVE_TABS_MAX: usize = 16;

/// What the window keeps of a tab that holds a view.
pub(super) struct Slot {
    /// Where the tab's engine posts.
    pub mailbox: Arc<Mailbox>,
    /// The page as the tab last had it on show (read when it comes to the front again).
    pub state: PageState,
    /// The page did not answer when the tab went behind.
    pub stuck: bool,
}

/// The tabs, their engines, and the strip that shows them.
pub(super) struct Set {
    pub model: Tabs,
    pub switch: Rc<Switch>,
    /// The front tab, as the notifiers of every mailbox (any thread) read it.
    pub front: Arc<AtomicU64>,
    pub slots: BTreeMap<TabId, Slot>,
    pub strip: Rc<TabStrip>,
    /// Downloads running, by the tab that started them (a tab with one is not discarded).
    downloads: BTreeMap<TabId, u32>,
}

impl Set {
    pub(super) fn new(widgets: &Widgets, switch: Rc<Switch>) -> Set {
        Set {
            model: Tabs::new(),
            switch,
            front: Arc::new(AtomicU64::new(0)),
            slots: BTreeMap::new(),
            strip: widgets.strip.get(),
            downloads: BTreeMap::new(),
        }
    }

    /// A download of `tab` started (`+1`) or ended (`-1`).
    pub(super) fn downloading(&mut self, tab: TabId, delta: i32) {
        let count = self.downloads.entry(tab).or_insert(0);
        *count = count.saturating_add_signed(delta);
        if *count == 0 {
            self.downloads.remove(&tab);
        }
    }

    pub(super) fn running_downloads(&self, tab: TabId) -> u32 {
        self.downloads.get(&tab).copied().unwrap_or(0)
    }
}

impl Browser {
    /// Opens the window's first tab: `url` (given on the command line), the tabs a crashed run left
    /// (`restore` chose to bring them back at once), or the start page.
    pub(super) fn start_tabs(
        &mut self,
        ui: &Ui<Msg>,
        url: Option<String>,
        restored: Option<session::Window>,
    ) {
        match (url, restored) {
            (Some(url), _) => self.first_tab(ui, &url),
            (None, Some(window)) => self.replace_tabs(ui, &window),
            (None, None) => {
                let start = self.internal.start();
                self.first_tab(ui, &start);
            }
        }
    }

    fn first_tab(&mut self, ui: &Ui<Msg>, url: &str) {
        let Some(id) = self.tabs.model.open(url, Place::End) else {
            return;
        };
        self.make_slot(ui, id, url, true);
        if let Some(raised) = self.tabs.model.raise(id) {
            self.adopt(ui, raised);
        }
        self.start_navigation(url);
    }

    /// Makes the engine of tab `id` (a view of webd, in the process its first address is on), and
    /// the mailbox whose news reaches the window as the front tab's or a background tab's.
    fn make_slot(&mut self, ui: &Ui<Msg>, id: TabId, url: &str, visible: bool) {
        let fresh = self.host.engines.make(id, &process_key(url));
        let (proxy, front) = (ui.proxy(), Arc::clone(&self.tabs.front));
        fresh.mailbox.set_notify(move |notice| {
            let msg = if front.load(Ordering::Relaxed) == id {
                match notice {
                    Notice::Frame => Msg::Frame,
                    Notice::State => Msg::State,
                    Notice::Events => Msg::Events,
                }
            } else if notice == Notice::Frame {
                // A tab behind the front one has no use for pixels.
                return;
            } else {
                Msg::Background(id)
            };
            let _ = proxy.send(msg);
        });
        self.tabs.switch.add(id, Rc::clone(&fresh.engine));
        self.tabs.slots.insert(
            id,
            Slot {
                mailbox: fresh.mailbox,
                state: PageState::default(),
                stuck: false,
            },
        );
        let engine = fresh.engine;
        // The engine's own agent is what a view is made with; another one the person chose is told.
        if let Some(preset) = crate::useragent::PRESETS
            .get(self.agent)
            .filter(|_| self.agent != 0)
        {
            engine.set_user_agent(preset.agent);
        }
        if !visible {
            engine.set_visible(false);
        }
        let (enabled, sites) = self.blocking_switch();
        engine.set_blocking(enabled, &sites);
        let page = ui.bounds(self.page.id());
        if page.width() > 0 && page.height() > 0 {
            engine.resize(page.width(), page.height());
        }
    }

    /// Lets go of tab `id`'s engine; its view ends with it.
    fn drop_slot(&mut self, id: TabId) {
        if let Some(answer) = self
            .dialogs
            .open_id()
            .filter(|dialog| crate::switch::split_id(*dialog).0 == id)
            .and_then(|_| self.dialogs.dismiss())
        {
            self.engine
                .dialog_reply(answer.id, answer.accepted, &answer.value);
        }
        self.tabs.slots.remove(&id);
        self.tabs.downloads.remove(&id);
        self.tabs.switch.remove(id);
        self.host.engines.gone(id);
    }

    /// The tab behind `raised` is in front in the model: the window follows. Returns whether the
    /// tab had no view and got one (the caller loads its address).
    fn adopt(&mut self, ui: &Ui<Msg>, raised: Raised) -> bool {
        if let Some(previous) = raised.previous {
            self.leave_front(previous);
            // What its page said just before it went behind is read now, as a tab behind would.
            self.background_news(ui, previous);
        }
        let id = raised.id;
        let mut made = false;
        if raised.needs_view && !raised.was_crashed && !self.tabs.slots.contains_key(&id) {
            let url = self.tabs.model.get(id).map(|t| t.url.clone());
            self.make_slot(ui, id, &url.unwrap_or_default(), true);
            made = true;
        }
        if raised.was_crashed {
            // Reload makes its view; coming to the front does not.
            self.tabs.model.set_crashed(id, true);
        }
        self.enter_front(ui, id);
        made
    }

    /// Tab `previous` goes behind: its page is kept for when it comes back, and the questions its
    /// page asked of the person are put away.
    fn leave_front(&mut self, previous: TabId) {
        if let Some(slot) = self.tabs.slots.get_mut(&previous) {
            slot.state = self.state.clone();
            slot.stuck = self.hang.stuck();
        }
        self.notifications.page_moved(self.engine.as_ref(), "");
        if let Some(engine) = self.tabs.switch.engine(previous) {
            engine.set_visible(false);
        }
    }

    /// Tab `id` is the one calls and news are about from now on.
    fn enter_front(&mut self, ui: &Ui<Msg>, id: TabId) {
        self.tabs.front.store(id, Ordering::Relaxed);
        self.tabs.switch.set_front(id);
        self.host.engines.front(id);
        let Some(slot) = self.tabs.slots.get(&id) else {
            return;
        };
        self.mailbox = Arc::clone(&slot.mailbox);
        let (saved, stuck) = (slot.state.clone(), slot.stuck);
        // The picture on show is the tab that went behind: blank until the tab's own frame comes.
        self.page.clear();
        xui_webkit::bands::take();
        self.engine.set_visible(true);
        let page = ui.bounds(self.page.id());
        if page.width() > 0 && page.height() > 0 {
            self.engine.resize(page.width(), page.height());
        }
        self.mailbox.request_full();
        self.hang.reset(stuck);
        self.show_tab(ui, saved);
    }

    /// Makes the tab behind `raised` the front one, and loads its address when it had no view.
    fn follow(&mut self, ui: &Ui<Msg>, raised: Raised) {
        let id = raised.id;
        if self.adopt(ui, raised) {
            let url = self.tabs.model.get(id).map(|t| t.url.clone());
            self.editing = false;
            self.start_navigation(&url.unwrap_or_default());
        }
    }

    /// Brings tab `id` to the front.
    pub(super) fn switch_to(&mut self, ui: &Ui<Msg>, id: TabId) {
        if self.tabs.model.active_id() == Some(id) {
            return;
        }
        let Some(raised) = self.tabs.model.raise(id) else {
            return;
        };
        self.follow(ui, raised);
        self.tab_event_marker("SWITCH");
        self.tabs_changed(ui);
    }

    /// A tab at `url`: in front (Ctrl+T, a page's new window) or behind the front one (a link opened
    /// with the middle button). The window holds [`crate::tabs::MAX_TABS`] at most.
    pub(super) fn new_tab(&mut self, ui: &Ui<Msg>, url: &str, place: Place, front: bool) {
        let Some(id) = self.tabs.model.open(url, place) else {
            self.set_status("There are too many tabs open");
            return;
        };
        self.make_slot(ui, id, url, front);
        if front {
            if let Some(raised) = self.tabs.model.raise(id) {
                self.adopt(ui, raised);
            }
            self.editing = false;
            self.start_navigation(url);
        } else if let Some(engine) = self.tabs.switch.engine(id) {
            engine.load(url);
            self.tabs.model.set_loading(id, true);
        }
        self.tab_event_marker("NEW");
        self.tabs_changed(ui);
    }

    /// Closes tab `id`; the last one closes the window.
    pub(super) fn close_tab(&mut self, ui: &Ui<Msg>, id: TabId) {
        if self.tabs.model.active_id() == Some(id) {
            self.notifications.page_moved(self.engine.as_ref(), "");
        }
        let Some(closure) = self.tabs.model.close(id) else {
            return;
        };
        self.drop_slot(id);
        self.tab_event_marker("CLOSE");
        if closure.last {
            self.save_tabs();
            ui.close();
            return;
        }
        if let Some(raised) = closure.front {
            self.follow(ui, raised);
        }
        self.tabs_changed(ui);
    }

    /// Ctrl+Shift+T.
    fn reopen_tab(&mut self, ui: &Ui<Msg>) {
        let Some(raised) = self.tabs.model.reopen() else {
            self.set_status("There is no closed tab to bring back");
            return;
        };
        self.follow(ui, raised);
        self.tab_event_marker("REOPEN");
        self.tabs_changed(ui);
    }

    /// What the strip, the keys and the link hooks ask.
    pub(super) fn tab_msg(&mut self, ui: &Ui<Msg>, msg: TabMsg) {
        match msg {
            TabMsg::Select(id) => self.switch_to(ui, id),
            TabMsg::Close(id) => self.close_tab(ui, id),
            TabMsg::New => {
                let start = self.internal.start();
                self.new_tab(ui, &start, Place::End, true);
                self.focus_address();
            }
            TabMsg::CloseFront => {
                if let Some(id) = self.tabs.model.active_id() {
                    self.close_tab(ui, id);
                }
            }
            TabMsg::Reopen => self.reopen_tab(ui),
            TabMsg::Next | TabMsg::Previous => {
                if let Some(raised) = self.tabs.model.cycle(msg == TabMsg::Next) {
                    self.follow(ui, raised);
                    self.tab_event_marker("SWITCH");
                    self.tabs_changed(ui);
                }
            }
            TabMsg::Jump(number) => {
                if let Some(raised) = self.tabs.model.jump(number) {
                    self.follow(ui, raised);
                    self.tab_event_marker("SWITCH");
                    self.tabs_changed(ui);
                }
            }
            TabMsg::OpenHovered => {
                let link = self.hover.clone();
                if crate::address::is_network(&link) {
                    self.new_tab(ui, &link, Place::BesideActive, false);
                }
            }
        }
    }

    /// Replaces every tab with those of a saved window, all discarded but the one that was in front,
    /// which loads.
    pub(super) fn replace_tabs(&mut self, ui: &Ui<Msg>, window: &session::Window) {
        for id in self.tabs.model.ids() {
            self.drop_slot(id);
        }
        self.tabs.model.replace_with(window);
        let front = self
            .tabs
            .model
            .id_at(window.active)
            .or_else(|| self.tabs.model.id_at(0));
        if let Some(raised) = front.and_then(|id| self.tabs.model.raise(id)) {
            self.follow(ui, raised);
        }
        self.tabs_changed(ui);
    }

    /// The tabs changed: lets the oldest used behind the front one give their views up past
    /// [`LIVE_TABS_MAX`], then shows the strip and keeps the session.
    fn tabs_changed(&mut self, ui: &Ui<Msg>) {
        for id in self.tabs.model.excess(LIVE_TABS_MAX) {
            if self.tabs.model.discard(id) {
                self.drop_slot(id);
                self.tab_event_marker("DISCARD");
            }
        }
        self.refresh_strip();
        self.save_tabs();
        self.announce(ui);
    }

    pub(super) fn tab_event_marker(&self, what: &str) {
        crate::marker(&format!("TAB:{what}:{}", self.tabs.model.len()));
    }
}
