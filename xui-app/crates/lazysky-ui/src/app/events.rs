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
                if self.settings.remember_handle {
                    self.settings.handle = Some(account.handle.clone());
                    self.save_settings();
                }
                self.account = Some(account);
                self.clear_everything();
                self.apply_visibility(ui);
                self.home.view.focus();
                self.last_unread_poll = None;
                self.refresh_timeline();
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
                        self.detail.set_title("Thread");
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
                            Some(Nav::Search { query, .. }) => format!("Search: {query}"),
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
            Event::Profile { seq, profile } => {
                self.pending = self.pending.saturating_sub(1);
                if let Some(Route::Profile { generation }) = self.routes.remove(&seq) {
                    if self.panes[DETAIL as usize].generation == generation {
                        self.profile_handle = Some(profile.actor.handle.clone());
                        self.detail.set_title(profile.actor.name());
                        let about = profile.description.lines().next().unwrap_or("");
                        self.detail.set_subtitle(&format!(
                            "@{}  {} posts  {} followers  {} following   {about}",
                            profile.actor.handle, profile.posts, profile.followers, profile.follows,
                        ));
                    }
                }
            }
            Event::Notifications { seq, items, .. } => {
                self.pending = self.pending.saturating_sub(1);
                if let Some(Route::Notifications { generation }) = self.routes.remove(&seq) {
                    if self.panes[DETAIL as usize].generation == generation {
                        let lines: Vec<String> = items
                            .iter()
                            .map(|n| {
                                let first = n.text.lines().next().unwrap_or("");
                                if first.is_empty() {
                                    n.summary()
                                } else {
                                    format!("{}: {first}", n.summary())
                                }
                            })
                            .collect();
                        self.people.show("Notifications", &lines);
                        self.notifications = items;
                        // Opening the list marks them seen.
                        let seq = self.seq();
                        self.ask(seq, Request::MarkSeen { seq }, Route::Ignore);
                        self.unread = 0;
                        self.home.set_unread(0);
                    }
                }
            }
            Event::Unread { seq, count } => {
                self.pending = self.pending.saturating_sub(1);
                self.routes.remove(&seq);
                self.unread = count;
                self.home.set_unread(count);
            }
            Event::Saved { path, bytes, .. } => {
                self.pending = self.pending.saturating_sub(1);
                self.say(&format!(
                    "Saved {} ({} KB).",
                    path.display(),
                    bytes.div_ceil(1000)
                ));
            }
            Event::BulkProgress {
                seq,
                scanned_posts,
                found,
                finished,
                scanning,
            } => {
                if self.bulk_seq == Some(seq) {
                    let line = if scanning {
                        format!("Scanning... {scanned_posts} posts, {found} pictures")
                    } else {
                        format!("Downloading {finished}/{found}")
                    };
                    self.detail.say(&line);
                }
            }
            Event::BulkDone {
                seq,
                saved,
                skipped,
                errors,
                cancelled,
            } => {
                self.pending = self.pending.saturating_sub(1);
                if self.bulk_seq == Some(seq) {
                    self.bulk_seq = None;
                    self.detail.set_bulk_running(ui, false);
                    let how = if cancelled { "Cancelled" } else { "Done" };
                    self.detail.say(&format!(
                        "{how}: {saved} saved, {skipped} already there, {errors} failed."
                    ));
                }
            }
            Event::Done { seq } => {
                self.pending = self.pending.saturating_sub(1);
                self.routes.remove(&seq);
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
                        self.refresh_timeline();
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
                let route = self.routes.remove(&seq);
                // Background requests fail quietly (an offline unread poll
                // must not keep writing to the status line).
                let quiet = matches!(route, Some(Route::Unread | Route::Ignore));
                match route {
                    Some(Route::Toggle { uri, before, .. }) => {
                        self.patch_everywhere(&uri, move |p| before.restore(p));
                    }
                    Some(Route::Compose) => self.composer.set_busy(false),
                    _ => {}
                }
                if !quiet || signed_out {
                    self.say(&reason);
                }
                if signed_out {
                    self.account = None;
                    self.clear_everything();
                    self.apply_visibility(ui);
                    self.login.say(&reason);
                    (self.report)(&Msg::Report(Report::SignedOut));
                }
            }
            Event::Status { line } => self.say(&line),
            Event::Image { key, result } => self.image_inbox.push_back((key, result)),
        }
    }
}
