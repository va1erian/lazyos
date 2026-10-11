//! The page stack: showing the page on top and loading its content.

use super::*;

impl LazySkyApp {
    pub(super) fn page_index(nav: &Nav) -> usize {
        match nav {
            Nav::Timeline => 1,
            Nav::Thread(_) | Nav::Profile { .. } | Nav::Bookmarks => 2,
            Nav::Search { posts: true, .. } => 2,
            Nav::People { .. } | Nav::Notifications | Nav::Search { posts: false, .. } => 3,
            Nav::Raw => 4,
            Nav::Composer => 5,
            Nav::Settings => 6,
            Nav::Viewer { .. } => 7,
        }
    }

    /// Shows the panel of the page on top (or sign-in when signed out).
    pub(super) fn apply_visibility(&self, ui: &Ui<Msg>) {
        let shown = if self.account.is_some() {
            self.nav.last().map_or(1, Self::page_index)
        } else {
            0
        };
        for (i, panel) in self.panels.iter().enumerate() {
            ui.set_visible(panel.id(), i == shown);
        }
    }

    /// The pane whose feed is on top, if one is.
    pub(super) fn active_pane(&self) -> Option<u8> {
        match Self::page_index(self.nav.last()?) {
            1 => Some(HOME),
            2 => Some(DETAIL),
            _ => None,
        }
    }

    pub(super) fn push(&mut self, ui: &mut Ui<Msg>, nav: Nav) {
        self.nav.push(nav);
        self.enter(ui);
    }

    pub(super) fn pop(&mut self, ui: &mut Ui<Msg>) {
        if self.nav.len() > 1 {
            self.nav.pop();
            self.enter(ui);
        }
    }

    /// Starts the detail pane over: late answers for what it showed are
    /// dropped, the old rows go, and a note says it is loading.
    fn reset_detail(&mut self, title: &str) {
        self.reset_pane(DETAIL);
        self.detail.set_title(title);
        {
            let mut s = self.detail.view.state.borrow_mut();
            s.feed.replace_all(Vec::new());
            s.note = "Loading...".into();
        }
        self.detail.view.invalidate();
    }

    /// Makes the page on top current: shows it and (re)loads its content.
    pub(super) fn enter(&mut self, ui: &mut Ui<Msg>) {
        let Some(nav) = self.nav.last().cloned() else {
            return;
        };
        self.people.set_toggle(ui, false, "");
        self.detail.set_extras(ui, false, false, "");
        match nav {
            Nav::Timeline => {
                self.home.view.focus();
                self.mark_moved(HOME);
            }
            Nav::Thread(uri) => {
                self.reset_detail("Thread");
                self.panes[DETAIL as usize].focus = Some(uri.clone());
                let generation = self.panes[DETAIL as usize].generation;
                let seq = self.seq();
                self.ask(
                    seq,
                    Request::Thread { seq, uri },
                    Route::Thread { generation },
                );
                self.detail.view.focus();
            }
            Nav::Profile { actor, tab } => {
                self.profile_handle = None;
                self.reset_detail("Profile");
                self.detail.set_extras(ui, true, false, "");
                let generation = self.panes[DETAIL as usize].generation;
                let seq = self.seq();
                self.ask(
                    seq,
                    Request::Profile {
                        seq,
                        actor: actor.clone(),
                    },
                    Route::Profile { generation },
                );
                let source = match tab {
                    1 => FeedSource::Author {
                        actor,
                        filter: AuthorFilter::Media,
                    },
                    2 => FeedSource::ActorLikes { actor },
                    _ => FeedSource::Author {
                        actor,
                        filter: AuthorFilter::Posts,
                    },
                };
                self.request_feed(DETAIL, source, None);
                self.detail.view.focus();
            }
            Nav::Bookmarks => {
                self.reset_detail("Bookmarks");
                self.request_feed(DETAIL, FeedSource::Bookmarks, None);
                self.detail.view.focus();
            }
            Nav::Search { query, posts } => {
                let toggle = if posts { "Show accounts" } else { "Show posts" };
                if posts {
                    self.reset_detail(&format!("Search: {query}"));
                    self.detail.set_extras(ui, false, true, toggle);
                    self.request_feed(DETAIL, FeedSource::SearchPosts { query }, None);
                    self.detail.view.focus();
                } else {
                    self.people.show(&format!("Search: {query}"), &[]);
                    self.people.set_toggle(ui, true, toggle);
                    self.people_lines.clear();
                    self.reset_pane(DETAIL);
                    let generation = self.panes[DETAIL as usize].generation;
                    let seq = self.seq();
                    self.ask(
                        seq,
                        Request::Actors {
                            seq,
                            list: ActorList::Search(query),
                            cursor: None,
                        },
                        Route::People { generation },
                    );
                }
            }
            Nav::People { list, title } => {
                self.people.show(&title, &[]);
                self.people_lines.clear();
                self.reset_pane(DETAIL);
                let generation = self.panes[DETAIL as usize].generation;
                let seq = self.seq();
                self.ask(
                    seq,
                    Request::Actors {
                        seq,
                        list,
                        cursor: None,
                    },
                    Route::People { generation },
                );
            }
            Nav::Notifications => {
                self.people.show("Notifications", &[]);
                self.notifications.clear();
                self.reset_pane(DETAIL);
                let generation = self.panes[DETAIL as usize].generation;
                let seq = self.seq();
                self.ask(
                    seq,
                    Request::Notifications { seq, cursor: None },
                    Route::Notifications { generation },
                );
            }
            Nav::Raw => {}
            Nav::Composer => {
                let name = self
                    .reply
                    .as_ref()
                    .map(|(_, parent)| parent.uri.clone())
                    .and_then(|uri| self.find_post(&uri))
                    .map(|post| post.author.handle);
                self.composer.reset(name.as_deref());
                self.pictures.clear();
                self.draft.clear();
            }
            Nav::Settings => self.settings_page.load(&self.settings),
            Nav::Viewer { uri, index } => self.open_viewer(&uri, index),
        }
        self.apply_visibility(ui);
        self.set_interval(ui, drain::BUSY_MS);
    }

    pub(super) fn mark_moved(&self, pane: u8) {
        let mut s = self.view(pane).state.borrow_mut();
        s.moved = true;
    }

    pub(super) fn find_post(&self, uri: &str) -> Option<Post> {
        [HOME, DETAIL].into_iter().find_map(|pane| {
            let s = self.view(pane).state.borrow();
            actions::find(&s, uri)
        })
    }
}
