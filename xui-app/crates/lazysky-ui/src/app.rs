//! The window: sign-in, then the timeline in a FeedView.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use lazysky_core::{Core, Event, Events, FeedSource, Platform, Request, Secret, Settings};
use xui_core::arrange::{column, panel, Handle, LayoutExt};
use xui_core::backend::{Result, TimerId};
use xui_core::widget::Panel;
use xui_core::{App, Ui};

use crate::drain;
use crate::feedview::{FeedAction, FeedView};
use crate::home::HomePage;
use crate::layouts::Layouts;
use crate::login::LoginPage;
use crate::prepare::Target;
use crate::timeline::{self, Staging};
use crate::{Msg, Report};

/// Layout results applied per tick (R5): more wait for the next one.
const REFRESH_PER_TICK: usize = 24;

pub struct LazySkyApp {
    core: Core,
    events: Events,
    layouts: Layouts,
    platform: Arc<dyn Platform>,
    login: LoginPage,
    home: HomePage,
    login_panel: Rc<Panel<Msg>>,
    home_panel: Rc<Panel<Msg>>,
    /// Requests sent and not yet answered.
    pending: usize,
    next_seq: u64,
    /// The request number of the page being loaded; older answers are
    /// dropped.
    feed_seq: u64,
    /// The cursor `Load more` will use.
    staging: Option<Staging>,
    timer: Option<(TimerId, u32)>,
    report: Box<dyn Fn(&Msg)>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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
        // The shaper is taken once, here, and lives on the layout thread.
        let layouts = Layouts::start(ui.text_shaper());
        let mut app = LazySkyApp {
            core,
            events,
            layouts,
            platform,
            login,
            home,
            login_panel,
            home_panel,
            pending: 0,
            next_seq: 1,
            feed_seq: 0,
            staging: None,
            timer: None,
            report: Box::new(report),
        };
        app.login.focus();
        app.set_interval(ui, drain::IDLE_MS);
        Ok(app)
    }

    /// The timeline view (tests drive it).
    pub fn feed_view(&self) -> Rc<FeedView> {
        Rc::clone(&self.home.view)
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
        if signed_in {
            self.home.view.focus();
        }
    }

    fn request_feed(&mut self, cursor: Option<String>) {
        let seq = self.seq();
        self.feed_seq = seq;
        self.send(Request::Feed {
            seq,
            source: FeedSource::Timeline,
            cursor,
        });
    }

    fn on_event(&mut self, ui: &mut Ui<Msg>, event: Event) {
        match event {
            Event::LoggedIn { account, .. } => {
                self.pending = self.pending.saturating_sub(1);
                self.login.set_busy(false);
                self.login.say("");
                self.home.set_account(&account.handle);
                self.clear_feed();
                self.show(ui, true);
                self.request_feed(None);
            }
            Event::LoginFailed { reason, .. } => {
                self.pending = self.pending.saturating_sub(1);
                self.login.set_busy(false);
                self.login.say(&reason);
            }
            Event::LoggedOut { .. } => {
                self.pending = self.pending.saturating_sub(1);
                self.clear_feed();
                self.show(ui, false);
                self.login.say("Signed out.");
                self.login.focus();
            }
            Event::Feed {
                seq, cursor, page, ..
            } => {
                self.pending = self.pending.saturating_sub(1);
                if seq != self.feed_seq {
                    return;
                }
                let ctx = {
                    let s = self.home.view.state.borrow();
                    timeline::context(&s, now_unix())
                };
                let staging = Staging::start(&self.layouts, seq, cursor.is_none(), page, ctx);
                self.staging = Some(staging);
                self.finish_staging_if_ready();
            }
            Event::Failed {
                reason, signed_out, ..
            } => {
                self.pending = self.pending.saturating_sub(1);
                self.home.say(&reason);
                if signed_out {
                    self.show(ui, false);
                    self.login.say(&reason);
                }
            }
            Event::Status { line } => self.home.say(&line),
        }
    }

    fn clear_feed(&mut self) {
        self.staging = None;
        let mut s = self.home.view.state.borrow_mut();
        s.feed.replace_all(Vec::new());
        s.note = String::new();
        drop(s);
        self.home.view.invalidate();
    }

    /// Inserts the staged page once every row of it is shaped.
    fn finish_staging_if_ready(&mut self) {
        if !self.staging.as_ref().is_some_and(Staging::is_complete) {
            return;
        }
        let Some(staging) = self.staging.take() else {
            return;
        };
        let (rows, replace) = staging.into_rows();
        let total = {
            let mut s = self.home.view.state.borrow_mut();
            if replace {
                s.feed.replace_all(rows);
            } else {
                s.feed.append(rows);
            }
            s.moved = true;
            if s.feed.is_empty() {
                s.note = "Nothing here yet.".into();
            }
            s.feed
                .rows()
                .iter()
                .filter(|r| matches!(r.kind, lazysky_core::feed::RowKind::Post(_)))
                .count()
        };
        self.home.say("");
        self.home.view.invalidate();
        (self.report)(&Msg::Report(Report::FeedRows(total)));
    }

    /// One timer tick: drain what finished, keep the rows near the view
    /// prepared, report timings.
    fn tick(&mut self, ui: &mut Ui<Msg>) {
        let started = Instant::now();
        let drained = drain::drain(&self.events, drain::BUDGET);
        for event in drained.events {
            self.on_event(ui, event);
        }
        // Layout results: this page's rows complete the staged page; the
        // rest re-prepare rows already in the feed.
        let mut refreshed = Vec::new();
        let mut taken = 0;
        while taken < REFRESH_PER_TICK {
            let Some(done) = self.layouts.try_next() else {
                break;
            };
            taken += 1;
            if let Some(staging) = self.staging.as_mut() {
                if staging.seq == done.tag && done.urgent {
                    staging.accept(done);
                    continue;
                }
            }
            if !done.urgent {
                refreshed.push(done);
            }
        }
        self.finish_staging_if_ready();
        let mut changed = false;
        {
            let mut s = self.home.view.state.borrow_mut();
            if !refreshed.is_empty() {
                changed |= timeline::apply_refresh(&mut s, refreshed);
            }
            if s.moved || s.relayout {
                timeline::maintain(&mut s, &self.layouts, now_unix());
            }
        }
        self.home.view.autoscroll_step();
        if changed {
            self.home.view.invalidate();
        }
        let busy = self.pending > 0
            || self.layouts.pending() > 0
            || self.staging.is_some()
            || self.home.view.autoscrolling()
            || taken == REFRESH_PER_TICK;
        let more = drained.cut_short || busy;
        let now = Instant::now();
        {
            let mut perf = self.home.view.perf.borrow_mut();
            perf.tick(started.elapsed());
            if let Some(summary) = perf.due(now) {
                self.platform.log(&format!("SKY:PERF:{}", summary.fields()));
            }
        }
        self.set_interval(ui, drain::interval(0, more));
    }

    fn click(&mut self, action: FeedAction) {
        match action {
            FeedAction::Target { target, .. } => match target {
                Target::Link(url) => {
                    if let Err(why) = self.platform.open_url(&url) {
                        self.home.say(&format!("Cannot open {url}: {why}"));
                    }
                }
                Target::LoadMore => self.load_more(),
                // Profiles, pictures and the action buttons arrive with
                // their phases (P4 to P6).
                _ => {}
            },
            FeedAction::Row(_) => {}
        }
    }

    fn load_more(&mut self) {
        if self.staging.is_some() {
            return;
        }
        let cursor = {
            let s = self.home.view.state.borrow();
            match s.feed.rows().last().map(|r| &r.kind) {
                Some(lazysky_core::feed::RowKind::LoadMore { cursor }) => Some(cursor.clone()),
                _ => None,
            }
        };
        if let Some(cursor) = cursor {
            self.home.say("Loading...");
            self.request_feed(Some(cursor));
        }
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
                self.load_more();
                self.set_interval(ui, drain::BUSY_MS);
            }
            Msg::Feed(action) => {
                self.click(action);
                self.set_interval(ui, drain::BUSY_MS);
            }
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
