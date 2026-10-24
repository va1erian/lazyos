//! A window's tabs as the session file keeps them, and the tabs a saved window brings back.

use super::{Gone, Tab, Tabs, MAX_TABS};
use crate::address;
use crate::session::{self, MAX_CLOSED};

impl Tabs {
    /// The window as the session file keeps it: the web pages, the one in front, the closed tabs.
    pub fn to_window(&self) -> session::Window {
        let kept: Vec<&Tab> = self
            .list
            .iter()
            .filter(|t| address::is_network(&t.url))
            .collect();
        let front = self
            .active
            .and_then(|id| self.index_of(id))
            .map_or(0, |here| {
                // The first kept tab at or after the one in front, else the last before it.
                let ahead = self.list[here..]
                    .iter()
                    .find(|t| address::is_network(&t.url))
                    .and_then(|t| kept.iter().position(|k| k.id == t.id));
                ahead.unwrap_or(kept.len().saturating_sub(1))
            });
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
        }
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
        for (at, gone) in window.closed.iter().take(MAX_CLOSED).enumerate() {
            self.closed.push_back(Gone {
                url: gone.url.clone(),
                title: gone.title.clone(),
                at: self.list.len() + at,
            });
        }
    }
}
