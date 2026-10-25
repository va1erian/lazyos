//! A window's tabs as the session file keeps them, and the tabs a saved window brings back.

use super::{Gone, Tab, Tabs, MAX_TABS};
use crate::address;
use crate::session::{self, MAX_CLOSED};

impl Tabs {
    /// The window as the session file keeps it: the web pages, the one in front, the closed tabs.
    ///
    /// The session format holds web pages only (`session::Session::parse` drops anything else), so
    /// a tab on one of the browser's own pages (the start page, `about:`) is left out; when that
    /// is the front tab, `active` is the **nearest** kept tab (the one after it on a tie), so the
    /// next start brings back the page the person was closest to rather than the first one. A
    /// closed tab's place is counted among the kept tabs for the same reason.
    pub fn to_window(&self) -> session::Window {
        let kept: Vec<&Tab> = self
            .list
            .iter()
            .filter(|t| address::is_network(&t.url))
            .collect();
        let front = self
            .active
            .and_then(|id| self.index_of(id))
            .map_or(0, |here| self.nearest_kept(here));
        session::Window {
            tabs: kept
                .into_iter()
                .map(|t| session::Tab {
                    url: t.url.clone(),
                    title: session::clean_title(&t.title),
                })
                .collect(),
            active: front,
            closed: self
                .closed
                .iter()
                .map(|g| session::Tab {
                    url: g.url.clone(),
                    title: session::clean_title(&g.title),
                })
                .collect(),
            closed_at: self.closed.iter().map(|g| self.kept_before(g.at)).collect(),
        }
    }

    /// How many web pages are among the first `at` tabs.
    fn kept_before(&self, at: usize) -> usize {
        self.list
            .iter()
            .take(at)
            .filter(|t| address::is_network(&t.url))
            .count()
    }

    /// The place among the web pages of the one nearest to the tab at `here` (itself, when it is
    /// one; else the closer neighbour, the later on a tie); 0 when there are none.
    fn nearest_kept(&self, here: usize) -> usize {
        let kept = |at: usize| address::is_network(&self.list[at].url);
        let after = (here..self.list.len()).find(|at| kept(*at));
        let before = (0..here).rev().find(|at| kept(*at));
        let near = match (before, after) {
            (Some(b), Some(a)) if here - b < a - here => b,
            (_, Some(a)) => a,
            (Some(b), None) => b,
            (None, None) => return 0,
        };
        self.kept_before(near)
    }

    /// The session file's contents for this window (no windows when no tab is a web page).
    pub fn to_session(&self) -> session::Session {
        let window = self.to_window();
        session::Session {
            windows: if window.tabs.is_empty() {
                Vec::new()
            } else {
                vec![window]
            },
        }
    }

    /// The tabs of a saved window, all discarded; the one that was in front is not raised yet
    /// (the window raises it with [`Tabs::raise`] and loads it).
    pub fn restore(window: &session::Window) -> Tabs {
        let mut tabs = Tabs::new();
        tabs.replace_with(window);
        tabs
    }

    /// Makes the tabs those of a saved window ([`Tabs::restore`]), keeping the ids used so far
    /// unused: a late event for a tab that was here cannot reach a restored one.
    pub fn replace_with(&mut self, window: &session::Window) {
        self.list.clear();
        self.closed.clear();
        self.active = None;
        self.chain = None;
        for kept in window.tabs.iter().take(MAX_TABS) {
            let id = self.next;
            self.next += 1;
            self.clock += 1;
            let mut tab = self.fresh(id, &kept.url, &kept.title, self.clock);
            tab.discarded = true;
            self.list.push(tab);
        }
        for (n, gone) in window.closed.iter().take(MAX_CLOSED).enumerate() {
            // Where it was, as far as the file knew; else the end. A reopen clamps it again.
            let at = window.closed_at.get(n).copied().unwrap_or(usize::MAX);
            self.closed.push_back(Gone {
                url: gone.url.clone(),
                title: gone.title.clone(),
                at: at.min(self.list.len()),
            });
        }
    }
}
