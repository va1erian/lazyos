//! The window's pages other than sign-in: the timeline, the detail feed
//! (a thread), a people list, the Raw view and the composer. Each is built
//! once into its own panel; the shell shows one at a time.

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{
    build as create, button, column, label, list, multiline_edit, row, Handle, LayoutExt, Mounted,
};
use xui_core::backend::{Result, WidgetId};
use xui_core::widget::{Button, Label, ListView, MultilineEdit};
use xui_core::HasText;

use crate::feedview::FeedView;
use crate::images::ImageCache;
use crate::Msg;

/// The pane numbers a FeedView reports with its clicks.
pub const HOME: u8 = 0;
pub const DETAIL: u8 = 1;

/// The timeline: who is signed in, New post, Sign out.
pub struct Home {
    who: Rc<Label<Msg>>,
    status: Rc<Label<Msg>>,
    pub view: Rc<FeedView>,
    _mounted: Mounted<Msg>,
}

impl Home {
    pub fn build(ui: &Ui<Msg>, page: WidgetId, images: &Rc<RefCell<ImageCache>>) -> Result<Home> {
        let (who, status, view) = (Handle::new(), Handle::new(), Handle::new());
        let images = Rc::clone(images);
        let mounted = ui.mount_in(
            page,
            column().gap(0).children((
                row().padding(8).gap(8).children((
                    label("").title().bind(&who).fill(1),
                    label("").bind(&status),
                    button("New post").on_click(Msg::Compose(None)),
                    button("Sign out").on_click(Msg::SignOut),
                )),
                create(move |ui: &Ui<Msg>| FeedView::new(ui, Rc::clone(&images), HOME))
                    .bind(&view)
                    .fill(1),
            )),
        )?;
        Ok(Home {
            who: who.get(),
            status: status.get(),
            view: view.get(),
            _mounted: mounted,
        })
    }

    pub fn set_account(&self, handle: &str) {
        self.who.set_text(&format!("@{handle}"));
        self.status.set_text("");
    }

    pub fn say(&self, line: &str) {
        self.status.set_text(line);
    }
}

/// A feed under a header with Back and a title: a thread today, a profile,
/// bookmarks or search results later.
pub struct Detail {
    title: Rc<Label<Msg>>,
    status: Rc<Label<Msg>>,
    pub view: Rc<FeedView>,
    _mounted: Mounted<Msg>,
}

impl Detail {
    pub fn build(ui: &Ui<Msg>, page: WidgetId, images: &Rc<RefCell<ImageCache>>) -> Result<Detail> {
        let (title, status, view) = (Handle::new(), Handle::new(), Handle::new());
        let images = Rc::clone(images);
        let mounted = ui.mount_in(
            page,
            column().gap(0).children((
                row().padding(8).gap(8).children((
                    button("Back").on_click(Msg::Back),
                    label("").title().bind(&title).fill(1),
                    label("").bind(&status),
                )),
                create(move |ui: &Ui<Msg>| FeedView::new(ui, Rc::clone(&images), DETAIL))
                    .bind(&view)
                    .fill(1),
            )),
        )?;
        Ok(Detail {
            title: title.get(),
            status: status.get(),
            view: view.get(),
            _mounted: mounted,
        })
    }

    pub fn set_title(&self, title: &str) {
        self.title.set_text(title);
        self.status.set_text("");
    }

    pub fn say(&self, line: &str) {
        self.status.set_text(line);
    }
}

/// A list of accounts (who liked or reposted a post).
pub struct People {
    title: Rc<Label<Msg>>,
    list: Rc<ListView<Msg>>,
    _mounted: Mounted<Msg>,
}

impl People {
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<People> {
        let (title, people) = (Handle::new(), Handle::new());
        let mounted = ui.mount_in(
            page,
            column().gap(0).children((
                row().padding(8).gap(8).children((
                    button("Back").on_click(Msg::Back),
                    label("").title().bind(&title).fill(1),
                )),
                list()
                    .then(|l| l.multi_select(false))
                    .on_select(Msg::PersonSelected)
                    .bind(&people)
                    .fill(1),
            )),
        )?;
        Ok(People {
            title: title.get(),
            list: people.get(),
            _mounted: mounted,
        })
    }

    pub fn show(&self, title: &str, lines: &[String]) {
        self.title.set_text(title);
        let items: Vec<&str> = lines.iter().map(String::as_str).collect();
        self.list.set_items(&items);
    }
}

/// One post's JSON, indented.
pub struct RawPage {
    text: Rc<MultilineEdit<Msg>>,
    _mounted: Mounted<Msg>,
}

impl RawPage {
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<RawPage> {
        let text = Handle::new();
        let mounted = ui.mount_in(
            page,
            column().gap(0).children((
                row().padding(8).gap(8).children((
                    button("Back").on_click(Msg::Back),
                    label("Raw view").title().fill(1),
                )),
                multiline_edit().bind(&text).fill(1),
            )),
        )?;
        Ok(RawPage {
            text: text.get(),
            _mounted: mounted,
        })
    }

    pub fn show(&self, json: &str) {
        self.text.set_text(json);
    }
}

/// The composer: a new post or a reply, up to four pictures.
pub struct Composer {
    title: Rc<Label<Msg>>,
    text: Rc<MultilineEdit<Msg>>,
    count: Rc<Label<Msg>>,
    pictures: Rc<Label<Msg>>,
    message: Rc<Label<Msg>>,
    add: Rc<Button<Msg>>,
    post: Rc<Button<Msg>>,
    _mounted: Mounted<Msg>,
}

impl Composer {
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<Composer> {
        let (title, text, count, pictures, message, add, post) = (
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
        );
        let mounted = ui.mount_in(
            page,
            column().gap(0).children((
                row().padding(8).gap(8).children((
                    button("Cancel").on_click(Msg::Back),
                    label("").title().bind(&title).fill(1),
                    button("Post").on_click(Msg::ComposerPost).bind(&post),
                )),
                column()
                    .padding(8)
                    .gap(6)
                    .children((
                        multiline_edit()
                            .on_change(Msg::ComposerText)
                            .bind(&text)
                            .fill(1),
                        row().gap(8).children((
                            label("0 / 300").bind(&count),
                            label("").bind(&pictures).fill(1),
                            button("Add picture...")
                                .on_click(Msg::ComposerAddPicture)
                                .bind(&add),
                            button("Remove picture").on_click(Msg::ComposerRemovePicture),
                        )),
                        label("").bind(&message),
                    ))
                    .fill(1),
            )),
        )?;
        Ok(Composer {
            title: title.get(),
            text: text.get(),
            count: count.get(),
            pictures: pictures.get(),
            message: message.get(),
            add: add.get(),
            post: post.get(),
            _mounted: mounted,
        })
    }

    /// Opens it empty, as a new post (`None`) or a reply to `reply_to`.
    pub fn reset(&self, reply_to: Option<&str>) {
        self.title.set_text(&match reply_to {
            Some(handle) => format!("Reply to @{handle}"),
            None => "New post".to_string(),
        });
        self.text.set_text("");
        self.set_count(0);
        self.show_pictures(&[]);
        self.message.set_text("");
        self.set_busy(false);
        self.text.focus();
    }

    pub fn set_count(&self, characters: usize) {
        self.count.set_text(&format!(
            "{characters} / {}",
            lazysky_core::proto::record::MAX_POST_CHARS
        ));
    }

    pub fn show_pictures(&self, names: &[String]) {
        let line = if names.is_empty() {
            String::new()
        } else {
            format!("{} picture(s): {}", names.len(), names.join(", "))
        };
        self.pictures.set_text(&line);
        self.add
            .set_enabled(names.len() < lazysky_core::proto::record::MAX_IMAGES);
    }

    pub fn say(&self, line: &str) {
        self.message.set_text(line);
    }

    pub fn set_busy(&self, busy: bool) {
        self.post.set_enabled(!busy);
    }
}
