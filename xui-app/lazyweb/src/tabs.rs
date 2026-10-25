//! The window's tabs, with no engine in them (docs/lazyweb-plan.md 2.1, 2.2, 2.5): the order, the
//! active tab, the stack of closed tabs, which tab goes to the front on a close or a cycle, and which
//! background tab gives its view up when memory runs short. The window turns what this module
//! decides into engines (`app/tabs.rs`); a test drives the same decisions against a recording
//! engine per tab.
//!
//! A tab is **live** (the window holds a view of webd for it), **discarded** (it keeps its address
//! and title and has no view; the next time it comes to the front it loads again) or **crashed**
//! (its view was lost; the window shows a page of its own until Reload). Ids are never reused in a
//! run, so a late event for a closed tab cannot reach another one.
//!
//! Persistence ([`Tabs::to_session`], [`Tabs::restore`]) is the `session` module's format: only
//! web pages are kept, and every restored tab is discarded until it is focused, so a start never
//! loads a page that crashed the last run before it is asked for.

use std::collections::VecDeque;

use crate::address;
use crate::session::MAX_CLOSED;

/// The most tabs a window holds (webd's browser scope takes 64 views).
pub const MAX_TABS: usize = 64;

/// A tab's number, unique in the window's life.
pub type TabId = u64;

/// One tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub id: TabId,
    /// The address last committed (or asked for, before anything committed).
    pub url: String,
    pub title: String,
    /// No view: the tab was discarded or restored and has not been in front since.
    pub discarded: bool,
    /// The view was lost (`ViewLost`): the tab shows the crashed-tab page.
    pub crashed: bool,
    /// A load is under way (the strip's spinner).
    pub loading: bool,
    /// A dialog is open or a download is running: the tab must not be discarded.
    pub busy: bool,
    /// When the tab was last in front (or made), by [`Tabs`]' own clock.
    used: u64,
}

impl Tab {
    /// Whether a view of webd is held for the tab.
    pub fn is_live(&self) -> bool {
        !self.discarded && !self.crashed
    }
}

/// Where a new tab goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// After the last tab (Ctrl+T).
    End,
    /// Next to the active tab, after the ones already opened from it (a link opened in the
    /// background, a page's new window).
    BesideActive,
    /// Next to this tab (a page's new window, whichever tab is in front).
    After(TabId),
}

/// What a close did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Closure {
    pub closed: TabId,
    /// The tab now in front, when the closed one was (it needs a view when it was discarded).
    pub front: Option<Raised>,
    /// That was the last tab: the window closes.
    pub last: bool,
}

/// What bringing a tab to the front needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Raised {
    pub id: TabId,
    /// The tab before it.
    pub previous: Option<TabId>,
    /// The tab had no view (discarded or crashed): the window makes one and loads its address.
    pub needs_view: bool,
    /// The view had been lost: the tab shows the crashed-tab page until Reload, so the window
    /// keeps what it has and loads nothing.
    pub was_crashed: bool,
}

/// A tab that was closed and can come back.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Gone {
    url: String,
    title: String,
    /// Where it was.
    at: usize,
}

/// The window's tabs.
#[derive(Debug, Default)]
pub struct Tabs {
    list: Vec<Tab>,
    active: Option<TabId>,
    /// The tabs closed last, newest first.
    closed: VecDeque<Gone>,
    next: TabId,
    clock: u64,
    /// The last tab opened beside the active one since it came to the front: the next one goes
    /// after it, so links opened in a row keep their order.
    chain: Option<TabId>,
}

impl Tabs {
    /// No tabs.
    pub fn new() -> Tabs {
        Tabs::default()
    }

    /// One live tab at `url`, in front.
    pub fn with_first(url: &str) -> Tabs {
        let mut tabs = Tabs::new();
        if let Some(id) = tabs.open(url, Place::End) {
            tabs.raise(id);
        }
        tabs
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Tab> {
        self.list.iter()
    }

    pub fn ids(&self) -> Vec<TabId> {
        self.list.iter().map(|t| t.id).collect()
    }

    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.list.iter().find(|t| t.id == id)
    }

    fn get_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.list.iter_mut().find(|t| t.id == id)
    }

    /// Where `id` is in the strip.
    pub fn index_of(&self, id: TabId) -> Option<usize> {
        self.list.iter().position(|t| t.id == id)
    }

    pub fn active_id(&self) -> Option<TabId> {
        self.active
    }

    pub fn active(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.get(id))
    }

    /// The tabs a view is held for.
    pub fn live_count(&self) -> usize {
        self.list.iter().filter(|t| t.is_live()).count()
    }

    /// A tab at `url`, live, not in front. `None` when the window holds [`MAX_TABS`] already.
    pub fn open(&mut self, url: &str, place: Place) -> Option<TabId> {
        if self.list.len() >= MAX_TABS {
            return None;
        }
        let at = match place {
            Place::End => self.list.len(),
            Place::BesideActive => self
                .chain
                .or(self.active)
                .and_then(|id| self.index_of(id))
                .map_or(self.list.len(), |i| i + 1),
            Place::After(opener) => self.index_of(opener).map_or(self.list.len(), |i| i + 1),
        };
        let id = self.next;
        self.next += 1;
        self.clock += 1;
        let tab = self.fresh(id, url, "", self.clock);
        self.list.insert(at, tab);
        if place == Place::BesideActive {
            self.chain = Some(id);
        }
        Some(id)
    }

    fn fresh(&self, id: TabId, url: &str, title: &str, used: u64) -> Tab {
        Tab {
            id,
            url: url.to_owned(),
            title: title.to_owned(),
            discarded: false,
            crashed: false,
            loading: false,
            busy: false,
            used,
        }
    }

    /// Brings `id` to the front. A discarded or crashed tab becomes live again: the window makes
    /// its view ([`Raised::needs_view`]).
    pub fn raise(&mut self, id: TabId) -> Option<Raised> {
        let previous = self.active;
        self.clock += 1;
        let clock = self.clock;
        let tab = self.get_mut(id)?;
        let needs_view = !tab.is_live();
        let was_crashed = tab.crashed;
        tab.discarded = false;
        tab.crashed = false;
        tab.used = clock;
        self.active = Some(id);
        if previous != Some(id) {
            self.chain = None;
        }
        Some(Raised {
            id,
            previous: previous.filter(|p| *p != id),
            needs_view,
            was_crashed,
        })
    }

    /// The next tab (`forward`) or the previous one, wrapping round, in front. Ctrl+Tab.
    pub fn cycle(&mut self, forward: bool) -> Option<Raised> {
        let count = self.list.len();
        let here = self.active.and_then(|id| self.index_of(id))?;
        if count < 2 {
            return None;
        }
        let to = if forward {
            (here + 1) % count
        } else {
            (here + count - 1) % count
        };
        self.raise(self.list[to].id)
    }

    /// Ctrl+`number`: `1..=8` is that tab, `9` the last one. A number past the last tab does
    /// nothing, as in other browsers.
    pub fn jump(&mut self, number: u8) -> Option<Raised> {
        let to = match number {
            1..=8 => usize::from(number) - 1,
            9 => self.list.len().checked_sub(1)?,
            _ => return None,
        };
        let id = self.list.get(to)?.id;
        self.raise(id)
    }

    /// Closes `id`. The tab to its right (else the one to its left) takes the front when `id` was
    /// in front. An address a page was on is kept for [`Tabs::reopen`].
    pub fn close(&mut self, id: TabId) -> Option<Closure> {
        let at = self.index_of(id)?;
        let tab = self.list.remove(at);
        if address::is_network(&tab.url) {
            self.closed.push_front(Gone {
                url: tab.url,
                title: tab.title,
                at,
            });
            self.closed.truncate(MAX_CLOSED);
        }
        if self.chain == Some(id) {
            self.chain = None;
        }
        let was_front = self.active == Some(id);
        let last = self.list.is_empty();
        let mut front = None;
        if was_front {
            self.active = None;
            let next = self.list.get(at).or_else(|| self.list.last()).map(|t| t.id);
            front = next.and_then(|next| self.raise(next));
        }
        Some(Closure {
            closed: id,
            front,
            last,
        })
    }

    /// Ctrl+Shift+T: the tab closed last comes back where it was, in front, and needs its view.
    pub fn reopen(&mut self) -> Option<Raised> {
        let gone = self.closed.pop_front()?;
        if self.list.len() >= MAX_TABS {
            self.closed.push_front(gone);
            return None;
        }
        let id = self.next;
        self.next += 1;
        self.clock += 1;
        let at = gone.at.min(self.list.len());
        let mut tab = self.fresh(id, &gone.url, &gone.title, self.clock);
        tab.discarded = true;
        self.list.insert(at, tab);
        self.raise(id)
    }

    /// How many closed tabs can come back.
    pub fn closed_count(&self) -> usize {
        self.closed.len()
    }

    /// Moves `id` to place `to` of the strip (dragging a tab).
    pub fn move_to(&mut self, id: TabId, to: usize) -> bool {
        let Some(from) = self.index_of(id) else {
            return false;
        };
        let tab = self.list.remove(from);
        self.list.insert(to.min(self.list.len()), tab);
        true
    }

    /// The page on show in `id` changed: its address (when it has one) and title.
    pub fn note(&mut self, id: TabId, url: &str, title: &str) {
        if let Some(tab) = self.get_mut(id) {
            if !url.is_empty() {
                url.clone_into(&mut tab.url);
            }
            title.clone_into(&mut tab.title);
        }
    }

    pub fn set_loading(&mut self, id: TabId, loading: bool) {
        if let Some(tab) = self.get_mut(id) {
            tab.loading = loading;
        }
    }

    /// A dialog is open, or a download runs, in `id`: it is not discarded meanwhile.
    pub fn set_busy(&mut self, id: TabId, busy: bool) {
        if let Some(tab) = self.get_mut(id) {
            tab.busy = busy;
        }
    }

    /// The view of `id` was lost (`true`): the tab keeps its address and shows the crashed-tab
    /// page. A new view of it (`false`) ends that.
    pub fn set_crashed(&mut self, id: TabId, crashed: bool) {
        if let Some(tab) = self.get_mut(id) {
            tab.crashed = crashed;
            if crashed {
                tab.loading = false;
            }
        }
    }

    /// Whether `id` may give its view up: live, not in front, with no dialog or download.
    pub fn can_discard(&self, id: TabId) -> bool {
        self.get(id)
            .is_some_and(|t| t.is_live() && !t.busy && self.active != Some(id))
    }

    /// Gives `id`'s view up; it keeps its address and title. `false` when it may not
    /// ([`Tabs::can_discard`]).
    pub fn discard(&mut self, id: TabId) -> bool {
        if !self.can_discard(id) {
            return false;
        }
        if let Some(tab) = self.get_mut(id) {
            tab.discarded = true;
            tab.loading = false;
        }
        true
    }

    /// The tab to discard first: the one used longest ago of those that may be.
    pub fn discard_choice(&self) -> Option<TabId> {
        self.list
            .iter()
            .filter(|t| self.can_discard(t.id))
            .min_by_key(|t| t.used)
            .map(|t| t.id)
    }

    /// The tabs to discard so that no more than `keep_live` hold a view, oldest-used first. Fewer
    /// when the rest may not be discarded (the one in front counts as live).
    pub fn excess(&self, keep_live: usize) -> Vec<TabId> {
        let over = self.live_count().saturating_sub(keep_live);
        let mut candidates: Vec<&Tab> = self
            .list
            .iter()
            .filter(|t| self.can_discard(t.id))
            .collect();
        candidates.sort_by_key(|t| t.used);
        candidates.into_iter().take(over).map(|t| t.id).collect()
    }

    /// The id of the tab at place `index` of the strip.
    pub fn id_at(&self, index: usize) -> Option<TabId> {
        self.list.get(index).map(|t| t.id)
    }
}

mod key;
mod persist;

pub use key::process_key;
