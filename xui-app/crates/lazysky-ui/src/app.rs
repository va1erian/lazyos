//! The window: sign-in, then a stack of pages over the timeline.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lazysky_core::feed::{RowId, RowKind};
use lazysky_core::proto::{Actor, FeedItem, Notification, Post, StrongRef};
use lazysky_core::{
    Account, ActorList, AuthorFilter, Bitmap, Core, Event, Events, FeedSource, ImageKey, Platform,
    Request, Secret, Settings,
};
use xui_core::arrange::{column, panel, Handle, LayoutExt};
use xui_core::backend::{Result, TimerId, WidgetId};
use xui_core::widget::{FileDialog, Menu, MenuId, Panel};
use xui_core::{App, Theme, Ui};

use crate::actions::{self, Before, Toggle, PENDING};
use crate::drain;
use crate::feedview::{FeedAction, FeedView};
use crate::images::ImageCache;
use crate::layouts::Layouts;
use crate::login::LoginPage;
use crate::pages::{Composer, Detail, Home, People, RawPage, SettingsPage, DETAIL, HOME};
use crate::prepare::{Action, Target};
use crate::timeline::{self, Staging};
use crate::{Msg, Report};

/// Layout results applied per tick (R5): more wait for the next one.
const REFRESH_PER_TICK: usize = 24;
/// New pictures made drawable per tick (R5).
const IMAGES_PER_TICK: usize = 6;

const MENU_RAW: MenuId = MenuId::new(0);
const MENU_LINK: MenuId = MenuId::new(1);
const MENU_LIKES: MenuId = MenuId::new(2);
const MENU_REPOSTS: MenuId = MenuId::new(3);
const MENU_DELETE: MenuId = MenuId::new(4);

/// A page of the window. The bottom of the stack is always the timeline.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Nav {
    Timeline,
    Thread(String),
    People {
        list: ActorList,
        title: String,
    },
    Raw,
    Composer,
    Profile {
        actor: String,
        tab: usize,
    },
    Bookmarks,
    Notifications,
    /// A search: accounts (`posts: false`) or posts.
    Search {
        query: String,
        posts: bool,
    },
    Settings,
}

/// How often the unread count is asked for.
const UNREAD_EVERY: Duration = Duration::from_secs(60);

/// What an answer from the core is for.
enum Route {
    Feed {
        pane: u8,
        generation: u64,
    },
    Thread {
        generation: u64,
    },
    People {
        generation: u64,
    },
    Raw,
    Toggle {
        uri: String,
        which: Toggle,
        before: Before,
    },
    Delete,
    Compose,
    Profile {
        generation: u64,
    },
    Notifications {
        generation: u64,
    },
    Unread,
    /// An answer nobody waits for (seen, configured, saved).
    Ignore,
}

/// A feed pane's loading state.
#[derive(Default)]
struct PaneState {
    staging: Option<Staging>,
    /// Bumped when the pane starts over, so late answers are dropped.
    generation: u64,
    /// Where "Load more" asks for the next page.
    source: Option<FeedSource>,
    /// The post to bring into view once the page is in (a thread's focus).
    focus: Option<String>,
}

pub struct LazySkyApp {
    core: Core,
    events: Events,
    layouts: Layouts,
    platform: Arc<dyn Platform>,
    images: Rc<RefCell<ImageCache>>,
    login: LoginPage,
    home: Home,
    detail: Detail,
    people: People,
    raw: RawPage,
    composer: Composer,
    settings_page: SettingsPage,
    /// `[login, home, detail, people, raw, composer, settings]`.
    panels: Vec<Rc<Panel<Msg>>>,
    account: Option<Account>,
    nav: Vec<Nav>,
    panes: [PaneState; 2],
    routes: HashMap<u64, Route>,
    /// Requests sent and not yet answered.
    pending: usize,
    next_seq: u64,
    /// Decoded pictures that arrived and wait their turn (R5).
    image_inbox: VecDeque<(ImageKey, std::result::Result<Bitmap, String>)>,
    /// The wanted pictures may have changed.
    images_dirty: bool,
    menu: Menu<Msg>,
    /// The post the open "..." menu is about: `(pane, post URI)`.
    menu_target: Option<(u8, String)>,
    /// The people list's accounts, for opening a profile later.
    people_lines: Vec<Actor>,
    picker: FileDialog<Msg>,
    pictures: Vec<PathBuf>,
    /// What the composer's text field holds (kept from its change events).
    draft: String,
    reply: Option<(StrongRef, StrongRef)>,
    settings: Settings,
    /// The notifications on the page, for opening one.
    notifications: Vec<Notification>,
    unread: u64,
    last_unread_poll: Option<Instant>,
    /// What the search box holds.
    search_text: String,
    /// The theme the host gave the window (restored by "Follow the system").
    system_theme: Theme,
    timer: Option<(TimerId, u32)>,
    report: Box<dyn Fn(&Msg)>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

mod events;
mod feeds;
mod interact;
mod pages;
mod tick;

impl LazySkyApp {
    /// Builds the window in `ui`; `report` hears every [`Report`] and the
    /// quit.
    pub fn build(
        ui: &mut Ui<Msg>,
        platform: Arc<dyn Platform>,
        settings: Settings,
        report: impl Fn(&Msg) + 'static,
    ) -> Result<LazySkyApp> {
        let frames: Vec<Handle<Panel<Msg>>> = (0..7).map(|_| Handle::new()).collect();
        let mut root = column();
        for frame in &frames {
            root = root.child(panel(column()).plain().bind(frame).fill(1));
        }
        ui.root(root)?;
        let panels: Vec<Rc<Panel<Msg>>> = frames.iter().map(Handle::get).collect();
        let id = |i: usize| -> WidgetId { panels[i].id() };
        let images: Rc<RefCell<ImageCache>> = Rc::default();
        let login = LoginPage::build(ui, id(0))?;
        let home = Home::build(ui, id(1), &images)?;
        let detail = Detail::build(ui, id(2), &images)?;
        let people = People::build(ui, id(3))?;
        let raw = RawPage::build(ui, id(4))?;
        let composer = Composer::build(ui, id(5))?;
        let settings_page = SettingsPage::build(ui, id(6))?;
        let system_theme = ui.theme_handle().get();
        ui.on_close(|| Some(Msg::Quit));
        ui.on_timer(|_| Some(Msg::Tick));
        let menu = Menu::context(ui)
            .build(|scope| {
                scope.item(MENU_RAW, "Raw view");
                scope.item(MENU_LINK, "Copy link to post");
                scope.separator();
                scope.item(MENU_LIKES, "Liked by...");
                scope.item(MENU_REPOSTS, "Reposted by...");
                scope.separator();
                scope.item(MENU_DELETE, "Delete post");
            })
            .on_select(|id| Some(Msg::PostMenu(id)));
        let picker = FileDialog::open_file(ui, "Add picture")?
            .require_existing(true)
            .filter("Pictures", &["png", "jpg", "jpeg", "gif", "webp"])
            .filter("All files", &[])
            .on_accept(|path| Some(Msg::ComposerPicture(path)))
            .on_cancel(|| None);
        picker.set_initial_dir(platform.pictures_dir());
        let (core, events) = Core::start(Arc::clone(&platform), settings.clone());
        // The shaper is taken once, here, and lives on the layout thread.
        let layouts = Layouts::start(ui.text_shaper());
        let mut app = LazySkyApp {
            core,
            events,
            layouts,
            platform,
            images,
            login,
            home,
            detail,
            people,
            raw,
            composer,
            settings_page,
            panels,
            account: None,
            nav: vec![Nav::Timeline],
            panes: [PaneState::default(), PaneState::default()],
            routes: HashMap::new(),
            pending: 0,
            next_seq: 1,
            image_inbox: VecDeque::new(),
            images_dirty: false,
            menu,
            menu_target: None,
            people_lines: Vec::new(),
            picker,
            pictures: Vec::new(),
            draft: String::new(),
            reply: None,
            settings: settings.clone(),
            notifications: Vec::new(),
            unread: 0,
            last_unread_poll: None,
            search_text: String::new(),
            system_theme,
            timer: None,
            report: Box::new(report),
        };
        app.apply_theme(ui);
        app.apply_max_image_height();
        if let Some(handle) = &app.settings.handle {
            app.login.set_handle(handle);
        }
        app.apply_visibility(ui);
        app.login.focus();
        app.set_interval(ui, drain::IDLE_MS);
        Ok(app)
    }

    /// The timeline view (tests drive it).
    pub fn feed_view(&self) -> Rc<FeedView> {
        Rc::clone(&self.home.view)
    }

    fn view(&self, pane: u8) -> &Rc<FeedView> {
        if pane == HOME {
            &self.home.view
        } else {
            &self.detail.view
        }
    }

    fn seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    /// Sends `request` (made with `seq`) and remembers what its answer is
    /// for.
    fn ask(&mut self, seq: u64, request: Request, route: Route) {
        self.pending += 1;
        self.routes.insert(seq, route);
        self.core.send(request);
    }

    fn say(&self, line: &str) {
        match self.nav.last() {
            Some(Nav::Timeline) | None => self.home.say(line),
            Some(Nav::Composer) => self.composer.say(line),
            Some(Nav::Settings) => self.settings_page.say(line),
            Some(_) => self.detail.say(line),
        }
    }

    /// Runs the timer at `millis` (restarting it only when that changed).
    fn set_interval(&mut self, ui: &mut Ui<Msg>, millis: u32) {
        if self.timer.is_some_and(|(_, current)| current == millis) {
            return;
        }
        if let Some((timer, _)) = self.timer.take() {
            ui.kill_timer(timer);
        }
        self.timer = Some((ui.set_timer(millis), millis));
    }
}

impl App for LazySkyApp {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick => self.tick(ui),
            Msg::SignInClicked => {
                let (handle, password) = self.login.take_credentials();
                self.update(
                    Msg::Login {
                        handle,
                        password: Secret::new(password),
                    },
                    ui,
                );
            }
            Msg::Login { handle, password } => {
                self.login.set_busy(true);
                self.login.say("Signing in...");
                let seq = self.seq();
                self.pending += 1;
                self.core.send(Request::Login {
                    seq,
                    handle,
                    password,
                });
                self.set_interval(ui, drain::BUSY_MS);
            }
            Msg::SignOut => {
                let seq = self.seq();
                self.pending += 1;
                self.core.send(Request::Logout { seq });
                self.set_interval(ui, drain::BUSY_MS);
            }
            Msg::Feed(pane, action) => {
                self.click(ui, pane, action);
                self.set_interval(ui, drain::BUSY_MS);
            }
            Msg::Back => self.pop(ui),
            Msg::Compose(reply_to) => {
                let post = reply_to.and_then(|uri| self.find_post(&uri));
                self.open_composer(ui, post);
            }
            Msg::ComposerText(text) => {
                self.composer.set_count(text.chars().count());
                self.draft = text;
            }
            Msg::ComposerAddPicture => self.picker.open(),
            Msg::ComposerPicture(path) => {
                if self.pictures.len() < lazysky_core::proto::record::MAX_IMAGES {
                    self.pictures.push(path);
                }
                self.composer.show_pictures(&self.picture_names());
            }
            Msg::ComposerRemovePicture => {
                self.pictures.pop();
                self.composer.show_pictures(&self.picture_names());
            }
            Msg::ComposerPost => {
                self.composer_post();
                self.set_interval(ui, drain::BUSY_MS);
            }
            Msg::PostMenu(id) => {
                self.post_menu(ui, id);
                self.set_interval(ui, drain::BUSY_MS);
            }
            Msg::PersonSelected(index) => self.person_selected(ui, index),
            Msg::SearchText(text) => self.search_text = text,
            Msg::SearchGo => self.search(ui),
            Msg::SearchToggle => self.search_toggle(ui),
            Msg::OpenNotifications => self.push(ui, Nav::Notifications),
            Msg::OpenBookmarks => self.push(ui, Nav::Bookmarks),
            Msg::OpenProfile(actor) => {
                if let Some(actor) = actor.or_else(|| self.account.as_ref().map(|a| a.did.clone()))
                {
                    self.push(ui, Nav::Profile { actor, tab: 0 });
                }
            }
            Msg::ProfileTab(tab) => self.profile_tab(ui, tab),
            Msg::OpenSettings => self.push(ui, Nav::Settings),
            Msg::SettingsTheme(index) => self.settings_theme(ui, index),
            Msg::SettingsMaxHeight(height) => self.settings_max_height(height),
            Msg::SettingsApplyService => self.settings_service(),
            Msg::SettingsRemember(on) => self.settings_remember(on),
            Msg::Report(_) => (self.report)(&msg),
            Msg::Quit => {
                let total = self.home.view.perf.borrow().total_fields();
                self.platform.log(&format!("SKY:PERF:TOTAL:{total}"));
                (self.report)(&Msg::Quit);
                ui.quit();
            }
        }
    }
}
