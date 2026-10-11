//! What the core answers, routed by the request it answers.

use super::*;

impl LazySkyApp {
    pub(super) fn on_event(&mut self, ui: &mut Ui<Msg>, event: Event) {
        match event {
            Event::LoggedIn { account, .. } => {
                self.pending = self.pending.saturating_sub(1);
                self.login.set_busy(false);
                self.login.say("");
                self.home.set_account(&account.handle);
                self.account = Some(account);
                self.clear_everything();
                self.apply_visibility(ui);
                self.home.view.focus();
                self.request_timeline(None);
            }
            Event::LoginFailed { reason, .. } => {
                self.pending = self.pending.saturating_sub(1);
                self.login.set_busy(false);
                self.login.say(&reason);
            }
            Event::LoggedOut { .. } => {
                self.pending = self.pending.saturating_sub(1);
                self.account = None;
                self.clear_everything();
                self.apply_visibility(ui);
                self.login.say("Signed out.");
                self.login.focus();
            }
            Event::Feed {
                seq, cursor, page, ..
            } => {
                self.pending = self.pending.saturating_sub(1);
                if let Some(Route::Feed { pane, generation }) = self.routes.remove(&seq) {
                    if self.panes[pane as usize].generation == generation {
                        let replace = cursor.is_none();
                        let items = page.items;
                        self.stage(pane, replace, items, page.cursor);
                    }
                }
            }
            Event::Thread { seq, thread } => {
                self.pending = self.pending.saturating_sub(1);
                if let Some(Route::Thread { generation }) = self.routes.remove(&seq) {
                    if self.panes[DETAIL as usize].generation == generation {
                        let items = thread
                            .rows()
                            .into_iter()
                            .map(|(post, _)| FeedItem {
                                post: post.clone(),
                                reason: None,
                            })
                            .collect();
                        self.stage(DETAIL, true, items, None);
                    }
                }
            }
            Event::Actors { seq, actors, .. } => {
                self.pending = self.pending.saturating_sub(1);
                if let Some(Route::People { generation }) = self.routes.remove(&seq) {
                    if self.panes[DETAIL as usize].generation == generation {
                        let title = match self.nav.last() {
                            Some(Nav::People { title, .. }) => title.clone(),
                            _ => String::new(),
                        };
                        let lines: Vec<String> = actors
                            .iter()
                            .map(|a| format!("{}  @{}", a.name(), a.handle))
                            .collect();
                        self.people.show(&title, &lines);
                        self.people_lines = actors;
                    }
                }
            }
            Event::Raw { seq, text } => {
                self.pending = self.pending.saturating_sub(1);
                if matches!(self.routes.remove(&seq), Some(Route::Raw)) {
                    self.raw.show(&text);
                    self.push(ui, Nav::Raw);
                }
            }
            Event::Created { seq, uri, .. } => {
                self.pending = self.pending.saturating_sub(1);
                match self.routes.remove(&seq) {
                    Some(Route::Toggle {
                        uri: post, which, ..
                    }) => {
                        let record = uri;
                        self.patch_everywhere(&post, move |p| which.confirm(p, record.clone()));
                    }
                    Some(Route::Compose) => {
                        self.composer.set_busy(false);
                        self.pop(ui);
                        self.home.say("Posted.");
                        // The new post appears at the top of a fresh timeline.
                        self.request_timeline(None);
                    }
                    _ => {}
                }
            }
            Event::Deleted { seq, uri } => {
                self.pending = self.pending.saturating_sub(1);
                if let Some(Route::Delete) = self.routes.remove(&seq) {
                    self.remove_post(&uri);
                }
            }
            Event::Failed {
                seq,
                reason,
                signed_out,
            } => {
                self.pending = self.pending.saturating_sub(1);
                match self.routes.remove(&seq) {
                    Some(Route::Toggle { uri, before, .. }) => {
                        self.patch_everywhere(&uri, move |p| before.restore(p));
                    }
                    Some(Route::Compose) => self.composer.set_busy(false),
                    _ => {}
                }
                self.say(&reason);
                if signed_out {
                    self.account = None;
                    self.clear_everything();
                    self.apply_visibility(ui);
                    self.login.say(&reason);
                }
            }
            Event::Status { line } => self.say(&line),
            Event::Image { key, result } => self.image_inbox.push_back((key, result)),
        }
    }
}
