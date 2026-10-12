//! What the user does: likes, replies, menus, the composer.

use super::*;

impl LazySkyApp {
    pub(super) fn patch_everywhere(&mut self, uri: &str, edit: impl Fn(&mut Post)) {
        for pane in [HOME, DETAIL] {
            let mut s = self.view(pane).state.borrow_mut();
            if actions::patch(&mut s, uri, &edit) {
                drop(s);
                self.view(pane).invalidate();
            }
        }
    }

    pub(super) fn remove_post(&mut self, uri: &str) {
        for pane in [HOME, DETAIL] {
            let mut s = self.view(pane).state.borrow_mut();
            let ids: Vec<RowId> = s
                .feed
                .rows()
                .iter()
                .filter(|r| matches!(&r.kind, RowKind::Post(item) if item.post.uri == uri))
                .map(|r| r.id)
                .collect();
            for id in ids {
                s.feed.remove(id);
            }
            s.moved = true;
            drop(s);
            self.view(pane).invalidate();
        }
    }

    /// A like or repost button was pressed on `uri`.
    pub(super) fn toggle(&mut self, which: Toggle, uri: &str) {
        let Some(post) = self.find_post(uri) else {
            return;
        };
        let current = which.record(&post).cloned();
        if current.as_deref() == Some(PENDING) {
            return; // a request for this button is already out
        }
        let before = Before::of(&post);
        let seq = self.seq();
        match current {
            Some(record) => {
                // Undo: show it off now, delete the record.
                self.patch_everywhere(uri, move |p| which.show(p, false, None));
                self.ask(
                    seq,
                    Request::DeleteRecord { seq, uri: record },
                    Route::Toggle {
                        uri: uri.to_string(),
                        which,
                        before,
                    },
                );
            }
            None => {
                self.patch_everywhere(uri, move |p| which.show(p, true, Some(PENDING.into())));
                let subject = post.strong_ref();
                let request = match which {
                    Toggle::Like => Request::Like { seq, subject },
                    Toggle::Repost => Request::Repost { seq, subject },
                };
                self.ask(
                    seq,
                    request,
                    Route::Toggle {
                        uri: uri.to_string(),
                        which,
                        before,
                    },
                );
            }
        }
    }

    pub(super) fn row_post(&self, pane: u8, row: RowId) -> Option<Post> {
        let s = self.view(pane).state.borrow();
        let i = s.feed.index_of(row)?;
        match &s.feed.row(i)?.kind {
            RowKind::Post(item) => Some(item.post.clone()),
            RowKind::LoadMore { .. } => None,
        }
    }

    pub(super) fn click(&mut self, ui: &mut Ui<Msg>, pane: u8, action: FeedAction) {
        match action {
            FeedAction::Row(row) => {
                if let Some(post) = self.row_post(pane, row) {
                    // A row of the thread already open just keeps its place.
                    let open = matches!(self.nav.last(), Some(Nav::Thread(u)) if *u == post.uri);
                    if !open {
                        self.push(ui, Nav::Thread(post.uri));
                    }
                }
            }
            FeedAction::Context { row, target, at } => {
                if let (Target::Image(index), Some(post)) = (target, self.row_post(pane, row)) {
                    self.image_target = Some((post.uri, index));
                    let bounds = ui.bounds(self.view(pane).id());
                    self.image_menu
                        .show_context(bounds.left + at.0, bounds.top + at.1);
                }
            }
            FeedAction::Target { row, target, at } => match target {
                Target::Link(url) => {
                    if let Err(why) = self.platform.open_url(&url) {
                        self.say(&format!("Cannot open {url}: {why}"));
                    }
                }
                Target::LoadMore => self.load_more(pane),
                Target::Action(Action::Like) => {
                    if let Some(post) = self.row_post(pane, row) {
                        self.toggle(Toggle::Like, &post.uri);
                    }
                }
                Target::Action(Action::Repost) => {
                    if let Some(post) = self.row_post(pane, row) {
                        self.toggle(Toggle::Repost, &post.uri);
                    }
                }
                Target::Action(Action::Reply) => {
                    if let Some(post) = self.row_post(pane, row) {
                        self.open_composer(ui, Some(post));
                    }
                }
                Target::Action(Action::More) => {
                    if let Some(post) = self.row_post(pane, row) {
                        self.show_menu(ui, pane, &post, at);
                    }
                }
                Target::Mention(did) => self.push(ui, Nav::Profile { actor: did, tab: 0 }),
                Target::Tag(tag) => self.push(
                    ui,
                    Nav::Search {
                        query: format!("#{tag}"),
                        posts: true,
                    },
                ),
                Target::Image(index) => {
                    if let Some(post) = self.row_post(pane, row) {
                        self.push(
                            ui,
                            Nav::Viewer {
                                uri: post.uri,
                                index,
                            },
                        );
                    }
                }
            },
        }
    }

    pub(super) fn show_menu(&mut self, ui: &Ui<Msg>, pane: u8, post: &Post, at: (i32, i32)) {
        let own = self
            .account
            .as_ref()
            .is_some_and(|a| a.did == post.author.did);
        self.menu.set_enabled(MENU_DELETE, own);
        self.menu_target = Some((pane, post.uri.clone()));
        let bounds = ui.bounds(self.view(pane).id());
        self.menu
            .show_context(bounds.left + at.0, bounds.top + at.1);
    }

    pub(super) fn open_composer(&mut self, ui: &mut Ui<Msg>, reply_to: Option<Post>) {
        self.reply = reply_to.map(|post| (post.thread_root(), post.strong_ref()));
        if self.nav.last() == Some(&Nav::Composer) {
            return;
        }
        self.push(ui, Nav::Composer);
    }

    pub(super) fn post_menu(&mut self, ui: &mut Ui<Msg>, id: MenuId) {
        let Some((_, uri)) = self.menu_target.take() else {
            return;
        };
        let Some(post) = self.find_post(&uri) else {
            return;
        };
        match id {
            MENU_RAW => {
                let seq = self.seq();
                self.ask(
                    seq,
                    Request::Raw {
                        seq,
                        json: Arc::clone(&post.raw),
                    },
                    Route::Raw,
                );
            }
            MENU_LINK => {
                let link = format!(
                    "https://bsky.app/profile/{}/post/{}",
                    post.author.handle,
                    post.rkey()
                );
                ui.set_clipboard_text(&link);
                self.say("Link copied.");
            }
            MENU_LIKES => self.push(
                ui,
                Nav::People {
                    list: ActorList::LikedBy(post.uri),
                    title: "Liked by".into(),
                },
            ),
            MENU_REPOSTS => self.push(
                ui,
                Nav::People {
                    list: ActorList::RepostedBy(post.uri),
                    title: "Reposted by".into(),
                },
            ),
            MENU_DELETE => {
                let seq = self.seq();
                self.ask(
                    seq,
                    Request::DeleteRecord { seq, uri: post.uri },
                    Route::Delete,
                );
            }
            _ => {}
        }
    }

    pub(super) fn composer_post(&mut self) {
        let text = self.draft.clone();
        let seq = self.seq();
        self.composer.set_busy(true);
        self.composer.say("Posting...");
        self.ask(
            seq,
            Request::CreatePost {
                seq,
                text,
                reply: self.reply.clone(),
                pictures: self.pictures.clone(),
            },
            Route::Compose,
        );
    }

    pub(super) fn picture_names(&self) -> Vec<String> {
        self.pictures
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect()
    }
}

impl LazySkyApp {
    /// A row of the people list: an account opens its profile; a
    /// notification opens what it is about.
    pub(super) fn person_selected(&mut self, ui: &mut Ui<Msg>, index: usize) {
        if matches!(self.nav.last(), Some(Nav::Notifications)) {
            let Some(n) = self.notifications.get(index).cloned() else {
                return;
            };
            use lazysky_core::proto::NotificationKind as Kind;
            match n.kind {
                Kind::Follow => self.push(
                    ui,
                    Nav::Profile {
                        actor: n.author.did,
                        tab: 0,
                    },
                ),
                Kind::Like | Kind::Repost => {
                    if let Some(subject) = n.subject {
                        self.push(ui, Nav::Thread(subject));
                    }
                }
                _ => self.push(ui, Nav::Thread(n.uri)),
            }
            return;
        }
        if let Some(actor) = self.people_lines.get(index) {
            let did = actor.did.clone();
            self.push(ui, Nav::Profile { actor: did, tab: 0 });
        }
    }

    pub(super) fn search(&mut self, ui: &mut Ui<Msg>) {
        let query = self.search_text.trim().to_string();
        if query.is_empty() {
            self.home.say("Type something to search for.");
            return;
        }
        // Posts first for a hashtag, accounts otherwise.
        let posts = query.starts_with('#');
        self.push(ui, Nav::Search { query, posts });
    }

    pub(super) fn search_toggle(&mut self, ui: &mut Ui<Msg>) {
        if let Some(Nav::Search { query, posts }) = self.nav.last().cloned() {
            self.nav.pop();
            self.push(
                ui,
                Nav::Search {
                    query,
                    posts: !posts,
                },
            );
        }
    }

    pub(super) fn profile_tab(&mut self, ui: &mut Ui<Msg>, tab: usize) {
        if let Some(Nav::Profile { actor, .. }) = self.nav.last().cloned() {
            self.nav.pop();
            self.push(ui, Nav::Profile { actor, tab });
        }
    }
}
