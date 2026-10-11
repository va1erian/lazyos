//! The window's pages other than sign-in: the timeline, the detail feed
//! (a thread), a people list, the Raw view and the composer. Each is built
//! once into its own panel; the shell shows one at a time.

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{
    build as create, button, checkbox, column, combo_box, edit, label, list, multiline_edit,
    number_field, row, Handle, LayoutExt, Mounted,
};
use xui_core::backend::{Result, WidgetId};
use xui_core::widget::{
    Button, CheckBox, ComboBox, Edit, Label, ListView, MultilineEdit, NumberField,
};
use xui_core::HasText;

use crate::feedview::FeedView;
use crate::images::ImageCache;
use crate::Msg;

/// The pane numbers a FeedView reports with its clicks.
pub const HOME: u8 = 0;
pub const DETAIL: u8 = 1;

/// The timeline: who is signed in, the way to everything else.
pub struct Home {
    who: Rc<Label<Msg>>,
    status: Rc<Label<Msg>>,
    notifications: Rc<Button<Msg>>,
    pub view: Rc<FeedView>,
    _mounted: Mounted<Msg>,
}

impl Home {
    pub fn build(ui: &Ui<Msg>, page: WidgetId, images: &Rc<RefCell<ImageCache>>) -> Result<Home> {
        let (who, status, view, notifications) =
            (Handle::new(), Handle::new(), Handle::new(), Handle::new());
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
                row().padding(8).gap(8).children((
                    edit()
                        .placeholder("Search Bluesky")
                        .on_change(Msg::SearchText)
                        .width(220),
                    button("Search").on_click(Msg::SearchGo),
                    button("Notifications")
                        .on_click(Msg::OpenNotifications)
                        .bind(&notifications),
                    button("Bookmarks").on_click(Msg::OpenBookmarks),
                    button("Profile").on_click(Msg::OpenProfile(None)),
                    button("Settings").on_click(Msg::OpenSettings),
                )),
                create(move |ui: &Ui<Msg>| FeedView::new(ui, Rc::clone(&images), HOME))
                    .bind(&view)
                    .fill(1),
            )),
        )?;
        Ok(Home {
            who: who.get(),
            status: status.get(),
            notifications: notifications.get(),
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

    /// The Notifications button, with the unread count when there is one.
    pub fn set_unread(&self, unread: u64) {
        let text = if unread > 0 {
            format!("Notifications ({unread})")
        } else {
            "Notifications".to_string()
        };
        self.notifications.set_text(&text);
    }
}

/// A feed under a header with Back and a title: a thread, a profile,
/// bookmarks or search results.
pub struct Detail {
    title: Rc<Label<Msg>>,
    status: Rc<Label<Msg>>,
    subtitle: Rc<Label<Msg>>,
    /// Posts, Media, Likes: shown for a profile only.
    tabs: [Rc<Button<Msg>>; 3],
    /// Accounts / Posts: shown for a search only.
    toggle: Rc<Button<Msg>>,
    pub view: Rc<FeedView>,
    _mounted: Mounted<Msg>,
}

impl Detail {
    pub fn build(ui: &Ui<Msg>, page: WidgetId, images: &Rc<RefCell<ImageCache>>) -> Result<Detail> {
        let (title, status, subtitle, view, toggle) = (
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
        );
        let tabs: [Handle<Button<Msg>>; 3] = std::array::from_fn(|_| Handle::new());
        let images = Rc::clone(images);
        let mounted = ui.mount_in(
            page,
            column().gap(0).children((
                row().padding(8).gap(8).children((
                    button("Back").on_click(Msg::Back),
                    label("").title().bind(&title).fill(1),
                    label("").bind(&status),
                    button("Show posts")
                        .on_click(Msg::SearchToggle)
                        .bind(&toggle),
                )),
                row().padding(8).gap(8).children((
                    label("").bind(&subtitle).fill(1),
                    button("Posts").on_click(Msg::ProfileTab(0)).bind(&tabs[0]),
                    button("Media").on_click(Msg::ProfileTab(1)).bind(&tabs[1]),
                    button("Likes").on_click(Msg::ProfileTab(2)).bind(&tabs[2]),
                )),
                create(move |ui: &Ui<Msg>| FeedView::new(ui, Rc::clone(&images), DETAIL))
                    .bind(&view)
                    .fill(1),
            )),
        )?;
        let [t0, t1, t2] = tabs.map(|h| h.get());
        Ok(Detail {
            title: title.get(),
            status: status.get(),
            subtitle: subtitle.get(),
            tabs: [t0, t1, t2],
            toggle: toggle.get(),
            view: view.get(),
            _mounted: mounted,
        })
    }

    pub fn set_title(&self, title: &str) {
        self.title.set_text(title);
        self.status.set_text("");
        self.subtitle.set_text("");
    }

    pub fn set_subtitle(&self, text: &str) {
        self.subtitle.set_text(text);
    }

    pub fn say(&self, line: &str) {
        self.status.set_text(line);
    }

    /// Which of the page's extras show: the profile tabs, the search toggle.
    pub fn set_extras(&self, ui: &Ui<Msg>, profile: bool, search: bool, toggle_text: &str) {
        for tab in &self.tabs {
            ui.set_visible(tab.id(), profile);
        }
        ui.set_visible(self.toggle.id(), search);
        self.toggle.set_text(toggle_text);
    }
}

/// A list of accounts (who liked a post, search results) or of
/// notifications.
pub struct People {
    title: Rc<Label<Msg>>,
    list: Rc<ListView<Msg>>,
    toggle: Rc<Button<Msg>>,
    _mounted: Mounted<Msg>,
}

impl People {
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<People> {
        let (title, people, toggle) = (Handle::new(), Handle::new(), Handle::new());
        let mounted = ui.mount_in(
            page,
            column().gap(0).children((
                row().padding(8).gap(8).children((
                    button("Back").on_click(Msg::Back),
                    label("").title().bind(&title).fill(1),
                    button("Show posts")
                        .on_click(Msg::SearchToggle)
                        .bind(&toggle),
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
            toggle: toggle.get(),
            _mounted: mounted,
        })
    }

    pub fn show(&self, title: &str, lines: &[String]) {
        self.title.set_text(title);
        let items: Vec<&str> = lines.iter().map(String::as_str).collect();
        self.list.set_items(&items);
    }

    /// The search toggle: shown (with `text`) for a search only.
    pub fn set_toggle(&self, ui: &Ui<Msg>, shown: bool, text: &str) {
        ui.set_visible(self.toggle.id(), shown);
        self.toggle.set_text(text);
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

/// Theme, picture height, server, remembering the handle.
pub struct SettingsPage {
    theme: Rc<ComboBox<Msg>>,
    height: Rc<NumberField<Msg>>,
    service: Rc<Edit<Msg>>,
    remember: Rc<CheckBox<Msg>>,
    note: Rc<Label<Msg>>,
    _mounted: Mounted<Msg>,
}

impl SettingsPage {
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<SettingsPage> {
        let (theme, height, service, remember, note) = (
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
        );
        let range = lazysky_core::settings::MAX_IMAGE_HEIGHT;
        let mounted = ui.mount_in(
            page,
            column().gap(0).children((
                row().padding(8).gap(8).children((
                    button("Back").on_click(Msg::Back),
                    label("Settings").title().fill(1),
                )),
                column().padding(16).gap(10).children((
                    label("Theme"),
                    combo_box(&["Follow the system", "Light", "Dark"])
                        .on_select(Msg::SettingsTheme)
                        .bind(&theme)
                        .width(220),
                    label("Tallest picture in a post (pixels)"),
                    number_field(f64::from(*range.start()), f64::from(*range.end()), 20.0)
                        .on_change(|v| Msg::SettingsMaxHeight(v as i32))
                        .bind(&height)
                        .width(120),
                    label("Server"),
                    row().gap(8).children((
                        edit().bind(&service).width(320),
                        button("Use this server").on_click(Msg::SettingsApplyService),
                    )),
                    checkbox("Remember my handle (never the password)")
                        .on_toggle(Msg::SettingsRemember)
                        .bind(&remember),
                    label("").bind(&note),
                )),
            )),
        )?;
        Ok(SettingsPage {
            theme: theme.get(),
            height: height.get(),
            service: service.get(),
            remember: remember.get(),
            note: note.get(),
            _mounted: mounted,
        })
    }

    /// Shows `settings` in the controls.
    pub fn load(&self, settings: &lazysky_core::Settings) {
        let index = match settings.theme {
            lazysky_core::ThemeChoice::System => 0,
            lazysky_core::ThemeChoice::Light => 1,
            lazysky_core::ThemeChoice::Dark => 2,
        };
        self.theme.select(index);
        self.height.set_value(f64::from(settings.max_image_height));
        self.service.set_text(&settings.service);
        self.remember.set_checked(settings.remember_handle);
        self.note.set_text("");
    }

    pub fn say(&self, line: &str) {
        self.note.set_text(line);
    }

    /// The server as typed.
    pub fn typed_service(&self) -> String {
        self.service.text()
    }
}
