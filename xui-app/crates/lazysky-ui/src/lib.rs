//! LazySky's portable window (docs/lazysky-plan.md §2): xui code that
//! depends on `lazysky-core` and `xui-core` only. The LazyOS binary and the
//! desktop example (`examples/desktop.rs`) launch it and supply a
//! [`lazysky_core::Platform`]; nothing here may call an OS API.
//!
//! The window never waits on the network (R1): it sends [`Request`]s to the
//! core and a timer drains the [`Event`]s within a budget (R5), fast while
//! work is pending and slow when idle (R8).

pub mod drain;
mod home;
mod login;

use std::rc::Rc;
use std::sync::Arc;

use lazysky_core::proto::FeedItem;
use lazysky_core::{Core, Event, Events, FeedSource, Platform, Request, Secret, Settings};
use xui_core::arrange::{column, panel, Handle, LayoutExt};
use xui_core::backend::{Result, TimerId};
use xui_core::widget::Panel;
use xui_core::{App, Ui};

use home::HomePage;
use login::LoginPage;

/// The window's design size when a compositor lays the app out.
pub const WINDOW: (i32, i32) = (960, 640);

/// What the window tells its host (the binary prints it as serial evidence).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// The timeline now holds this many rows.
    FeedRows(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    /// The timer: drain the core's events.
    Tick,
    /// The Sign in button: read the fields.
    SignInClicked,
    /// Sign in with these credentials (the button's message, with the fields
    /// read; tests send it directly).
    Login {
        handle: String,
        password: Secret,
    },
    SignOut,
    LoadMore,
    Report(Report),
    Quit,
}

/// The timeline as loaded so far.
#[derive(Default)]
struct Timeline {
    items: Vec<FeedItem>,
    cursor: Option<String>,
    skipped: usize,
}

pub struct LazySkyApp {
    core: Core,
    events: Events,
    #[allow(dead_code)]
    platform: Arc<dyn Platform>,
    login: LoginPage,
    home: HomePage,
    login_panel: Rc<Panel<Msg>>,
    home_panel: Rc<Panel<Msg>>,
    /// Requests sent and not yet answered.
    pending: usize,
    next_seq: u64,
    /// The sequence number of the page being loaded; older answers are
    /// dropped.
    feed_seq: u64,
    timeline: Timeline,
    timer: Option<(TimerId, u32)>,
    report: Box<dyn Fn(&Msg)>,
}

impl LazySkyApp {
    /// Builds the window in `ui`; `report` hears every [`Report`] and the
    /// quit.
    pub fn build(
        ui: &mut Ui<Msg>,
        platform: Arc<dyn Platform>,
        settings: Settings,
        report: impl Fn(&Msg) + 'static,
    ) -> Result<LazySkyApp> {
        let (login_frame, home_frame) = (Handle::new(), Handle::new());
        ui.root(
            column()
                .child(panel(column()).plain().bind(&login_frame).fill(1))
                .child(panel(column()).plain().bind(&home_frame).fill(1)),
        )?;
        let (login_panel, home_panel) = (login_frame.get(), home_frame.get());
        let login = LoginPage::build(ui, login_panel.id())?;
        let home = HomePage::build(ui, home_panel.id())?;
        ui.set_visible(home_panel.id(), false);
        ui.on_close(|| Some(Msg::Quit));
        ui.on_timer(|_| Some(Msg::Tick));
        let (core, events) = Core::start(Arc::clone(&platform), settings);
        let mut app = LazySkyApp {
            core,
            events,
            platform,
            login,
            home,
            login_panel,
            home_panel,
            pending: 0,
            next_seq: 1,
            feed_seq: 0,
            timeline: Timeline::default(),
            timer: None,
            report: Box::new(report),
        };
        app.login.focus();
        app.set_interval(ui, drain::IDLE_MS);
        Ok(app)
    }

    fn seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    fn send(&mut self, request: Request) {
        self.pending += 1;
        self.core.send(request);
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

    fn show(&self, ui: &Ui<Msg>, signed_in: bool) {
        ui.set_visible(self.login_panel.id(), !signed_in);
        ui.set_visible(self.home_panel.id(), signed_in);
    }

    fn request_feed(&mut self, cursor: Option<String>) {
        let seq = self.seq();
        self.feed_seq = seq;
        self.home.set_loading(true);
        self.send(Request::Feed {
            seq,
            source: FeedSource::Timeline,
            cursor,
        });
    }

    fn on_event(&mut self, ui: &mut Ui<Msg>, event: Event) -> Option<Report> {
        match event {
            Event::LoggedIn { account, .. } => {
                self.pending = self.pending.saturating_sub(1);
                self.login.set_busy(false);
                self.login.say("");
                self.timeline = Timeline::default();
                self.home.set_account(&account.handle);
                self.show(ui, true);
                self.request_feed(None);
                None
            }
            Event::LoginFailed { reason, .. } => {
                self.pending = self.pending.saturating_sub(1);
                self.login.set_busy(false);
                self.login.say(&reason);
                None
            }
            Event::LoggedOut { .. } => {
                self.pending = self.pending.saturating_sub(1);
                self.timeline = Timeline::default();
                self.show(ui, false);
                self.login.say("Signed out.");
                self.login.focus();
                None
            }
            Event::Feed {
                seq, cursor, page, ..
            } => {
                self.pending = self.pending.saturating_sub(1);
                if seq != self.feed_seq {
                    return None;
                }
                if cursor.is_none() {
                    self.timeline = Timeline::default();
                }
                self.timeline.skipped += page.skipped;
                self.timeline.cursor = page.cursor;
                self.timeline.items.extend(page.items);
                self.home.set_feed(
                    &self.timeline.items,
                    self.timeline.skipped,
                    self.timeline.cursor.is_some(),
                );
                Some(Report::FeedRows(self.timeline.items.len()))
            }
            Event::Failed {
                reason, signed_out, ..
            } => {
                self.pending = self.pending.saturating_sub(1);
                self.home.set_loading(false);
                if signed_out {
                    self.show(ui, false);
                    self.login.say(&reason);
                }
                None
            }
            Event::Status { .. } => None,
        }
    }
}

impl App for LazySkyApp {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick => {
                let drained = drain::drain(&self.events, drain::BUDGET);
                for event in drained.events {
                    if let Some(report) = self.on_event(ui, event) {
                        (self.report)(&Msg::Report(report));
                    }
                }
                self.set_interval(ui, drain::interval(self.pending, drained.cut_short));
            }
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
                self.send(Request::Login {
                    seq,
                    handle,
                    password,
                });
                self.set_interval(ui, drain::BUSY_MS);
            }
            Msg::SignOut => {
                let seq = self.seq();
                self.send(Request::Logout { seq });
                self.set_interval(ui, drain::BUSY_MS);
            }
            Msg::LoadMore => {
                if let Some(cursor) = self.timeline.cursor.clone() {
                    self.request_feed(Some(cursor));
                    self.set_interval(ui, drain::BUSY_MS);
                }
            }
            Msg::Report(_) => (self.report)(&msg),
            Msg::Quit => {
                (self.report)(&Msg::Quit);
                ui.quit();
            }
        }
    }
}
