//! The page stack: showing the page on top and loading its content.

use super::*;

impl LazySkyApp {
    pub(super) fn page_index(nav: &Nav) -> usize {
        match nav {
            Nav::Timeline => 1,
            Nav::Thread(_) => 2,
            Nav::People { .. } => 3,
            Nav::Raw => 4,
            Nav::Composer => 5,
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
        match self.nav.last()? {
            Nav::Timeline => Some(HOME),
            Nav::Thread(_) => Some(DETAIL),
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

    /// Makes the page on top current: shows it and (re)loads its content.
    pub(super) fn enter(&mut self, ui: &mut Ui<Msg>) {
        let Some(nav) = self.nav.last().cloned() else {
            return;
        };
        match nav {
            Nav::Timeline => {
                self.home.view.focus();
                self.mark_moved(HOME);
            }
            Nav::Thread(uri) => {
                self.detail.set_title("Thread");
                let pane = &mut self.panes[DETAIL as usize];
                pane.generation += 1;
                pane.staging = None;
                pane.source = None;
                pane.focus = Some(uri.clone());
                let generation = pane.generation;
                {
                    let mut s = self.detail.view.state.borrow_mut();
                    s.feed.replace_all(Vec::new());
                    s.note = "Loading...".into();
                }
                self.detail.view.invalidate();
                let seq = self.seq();
                self.ask(
                    seq,
                    Request::Thread { seq, uri },
                    Route::Thread { generation },
                );
                self.detail.view.focus();
            }
            Nav::People { list, title } => {
                self.people.show(&title, &[]);
                self.people_lines.clear();
                let generation = self.panes[DETAIL as usize].generation + 1;
                self.panes[DETAIL as usize].generation = generation;
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
