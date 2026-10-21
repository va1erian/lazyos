//! The Topic monitor: the Playground's second window, "Messenger Topics
//! (developer tool)" (`docs/messenger-playground-plan.md` section 3).
//!
//! It is its own [`App`] (`Ui::open_window`) and owns everything about
//! listening: the [`crate::listen::Subs`] (one `Subscriber`, doorbell-driven),
//! the tab per subscription, the event details and the Publish form. Closing
//! the window drops it, and the drop ends every subscription (the unsubscribe
//! is retried as before; the subscriber's drop is the last try). The listening
//! rules are unchanged: only the doorbell look-out timer and the drain
//! continuation run, and neither calls the broker ([`crate::topics`]).
//!
//! What it asks of the main window it sends through the main window's
//! [`Proxy`]: a Publish is a [`Msg::PublishRequest`] (the main window runs the
//! one-line script, so there is still one execution path, D3), the strip's and
//! View menu's notice changes are [`Msg::NoticeChanged`].

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use xui_app::devtool::Notice;
use xui_app::probe;
use xui_app::trace::{self, Channel};
use xui_core::app::{App, Proxy, Ui};
use xui_core::backend::{Result, TimerId, WidgetId};
use xui_core::geometry::Rect;
use xui_core::{Key, Modifiers};
use xui_playground::listener::Registry;

use crate::app::{PubForm, SubPage};
use crate::listen::Subs;
use crate::msg::Msg;
use crate::tmsg::TMsg;
use crate::topicsui::TopicsWidgets;

/// The window's name; the title adds " (developer tool)".
pub const TITLE: &str = "Messenger Topics";
/// The window size in design pixels.
pub const WINDOW: (i32, i32) = (760, 620);
/// A probe line is printed this long after the layout last changed.
const PROBE_SETTLE_MS: u32 = 250;
/// The height of a `Tabs` header, in design pixels (xui's `tabs.rs`).
const TAB_BAR: i32 = 32;
/// A list's column-title row and a row of a list, in design pixels (xui's
/// `ListView`), and how many rows get a probe name.
const LIST_HEADER: i32 = 22;
const ROW_HEIGHT: i32 = 22;
const PROBE_ROWS: i32 = 6;

/// What the main window hands the monitor when it opens it.
pub struct Deps {
    pub main: Proxy<Msg>,
    /// The main window's clock, so publisher names age the same way.
    pub started: Instant,
    pub notice: Rc<Notice>,
    pub registry: Registry,
}

/// The timers the monitor owns; each exists only while it has work.
#[derive(Default)]
pub struct Timers {
    pub bell: Cell<Option<TimerId>>,
    pub drain: Cell<Option<TimerId>>,
    pub probe: Cell<Option<TimerId>>,
}

pub struct TopicsTool {
    pub w: TopicsWidgets,
    pub title: String,
    pub started: Instant,
    pub main: Proxy<Msg>,
    pub notice: Rc<Notice>,
    pub registry: Registry,
    pub subs: Subs,
    pub sub_pages: Vec<SubPage>,
    pub sub_welcome: bool,
    pub current_sub: usize,
    pub publish: PubForm,
    pub timers: Rc<Timers>,
    /// The layout changed: print the probe lines once it settles.
    pub probe_due: Cell<bool>,
    /// `TOPICS:UP` is printed once, with the first probe lines.
    announced: Cell<bool>,
}

impl TopicsTool {
    /// Build the window's widgets and its first probe timer.
    pub fn new(ui: &mut Ui<TMsg>, deps: Deps) -> Result<TopicsTool> {
        let w = TopicsWidgets::default();
        ui.root(w.layout(&deps.notice))?;
        ui.on_key(shortcut);
        ui.register_events(WidgetId::NONE, |event| {
            matches!(event, xui_core::backend::Event::Resize { .. }).then_some(TMsg::Resized)
        });
        let timers = Rc::new(Timers::default());
        timers.probe.set(Some(ui.set_timer(PROBE_SETTLE_MS)));
        {
            let timers = Rc::clone(&timers);
            ui.on_timer(move |id| {
                let is = |cell: &Cell<Option<TimerId>>| cell.get() == Some(id);
                if is(&timers.bell) {
                    Some(TMsg::Bell)
                } else if is(&timers.drain) {
                    Some(TMsg::Drain)
                } else if is(&timers.probe) {
                    Some(TMsg::ProbeDue)
                } else {
                    None
                }
            });
        }
        let tool = TopicsTool {
            w,
            title: xui_app::devtool::title(crate::topicswin::TITLE),
            started: deps.started,
            main: deps.main,
            notice: deps.notice,
            registry: deps.registry,
            subs: Subs::default(),
            sub_pages: Vec::new(),
            sub_welcome: true,
            current_sub: 0,
            publish: PubForm::default(),
            timers,
            probe_due: Cell::new(false),
            announced: Cell::new(false),
        };
        tool.w.strip.get().sync();
        tool.update_status();
        Ok(tool)
    }

    /// Tell the main window something.
    pub fn tell_main(&self, msg: Msg) {
        let _ = self.main.send(msg);
    }

    /// Milliseconds since the main window started (the models' clock).
    pub fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    pub fn note(&self, text: &str) {
        self.w.status.get().set_text(0, text);
    }

    /// Status bar part 1, and the count the main window shows.
    pub fn update_status(&self) {
        let n = self.subs.subs.len();
        self.w.status.get().set_text(1, &format!("listening: {n}"));
        self.tell_main(Msg::TopicsStatus(n));
    }

    pub fn probe_soon(&self) {
        self.probe_due.set(true);
    }

    /// Per-message housekeeping: the probe timer.
    fn after(&self, ui: &mut Ui<TMsg>) {
        if self.probe_due.replace(false) && self.timers.probe.get().is_none() {
            self.timers.probe.set(Some(ui.set_timer(PROBE_SETTLE_MS)));
        }
    }

    /// The settle timer fired: print where the named controls are (and, the
    /// first time, that the window is up).
    fn probe_now(&self, ui: &mut Ui<TMsg>) {
        if let Some(id) = self.timers.probe.take() {
            ui.kill_timer(id);
        }
        if probe::enabled() {
            self.say_controls(ui);
        }
        if !self.announced.replace(true) {
            trace::pass!(Channel::Playground, "TOPICS:UP");
        }
    }

    fn say_controls(&self, ui: &Ui<TMsg>) {
        let scale = ui.dpi() as i32 / 96;
        let title = self.title.clone();
        let say = |name: &str, r: Rect| {
            if r.right > r.left && r.bottom > r.top {
                probe::widget(&title, name, r.left, r.top, r.width(), r.height());
            }
        };
        let w = &self.w;
        w.strip.get().probe(ui, &title);
        for (name, id) in [
            ("sub_tabs", w.subs.get().id()),
            ("sub_filter", w.sub_filter.get().id()),
            ("pause_button", w.pause.get().id()),
            ("clear_button", w.clear.get().id()),
            ("pin_button", w.pin.get().id()),
            ("copy_button", w.copy.get().id()),
            ("save_button", w.save.get().id()),
            ("stop_listening", w.close_sub.get().id()),
            ("event_details", w.details.get().id()),
            ("pub_topic", w.pub_topic.get().id()),
            ("pub_payload", w.pub_payload.get().id()),
            ("pub_retained", w.pub_retained.get().id()),
            ("pub_go", w.pub_go.get().id()),
            ("pub_listen", w.pub_listen.get().id()),
        ] {
            say(name, ui.bounds(id));
        }
        // A tab page's children report coordinates relative to the page: the
        // events list sits under the tab strip.
        if let Some(sub) = self.sub_pages.get(self.current_sub) {
            let p = ui.bounds(w.subs.get().id());
            let c = ui.bounds(sub.list.get().id());
            let events = Rect::new(
                p.left + c.left,
                p.top + TAB_BAR * scale + c.top,
                p.left + c.right,
                p.top + TAB_BAR * scale + c.bottom,
            );
            say("events", events);
            for k in 0..PROBE_ROWS {
                let top = events.top + LIST_HEADER * scale + k * ROW_HEIGHT * scale;
                if top + ROW_HEIGHT * scale > events.bottom {
                    break;
                }
                say(
                    &format!("events_row:{k}"),
                    Rect::new(events.left, top, events.right, top + ROW_HEIGHT * scale),
                );
            }
        }
    }

    // ---- the notice ------------------------------------------------------

    fn notice_changed(&self) {
        self.w.strip.get().sync();
        self.probe_soon();
    }

    fn notice_dismiss(&self) {
        if let Some(note) = crate::notice::dismiss(&self.notice) {
            self.note(&note);
        }
        self.notice_changed();
        self.tell_main(Msg::NoticeChanged);
    }

    fn notice_show(&self) {
        if let Some(note) = crate::notice::show(&self.notice) {
            self.note(&note);
        }
        self.notice_changed();
        self.tell_main(Msg::NoticeChanged);
    }
}

/// The keys the monitor answers wherever the focus is.
pub fn shortcut(key: Key, modifiers: Modifiers) -> Option<TMsg> {
    let command = modifiers.ctrl || modifiers.win;
    match key {
        Key::W if command => Some(TMsg::Close),
        Key::Q if command => Some(TMsg::QuitApp),
        Key::D if command && modifiers.shift => Some(TMsg::NoticeShow),
        _ => None,
    }
}

impl Drop for TopicsTool {
    /// The window closed (or the main window did). The subscriptions end with
    /// the subscriber's own drop, which unsubscribes each with a poll call: a
    /// broker that answers slowly (or not at all) must not hold the UI for
    /// two seconds per tab. Only an explicit Stop listening waits and keeps a
    /// failed unsubscribe for a retry (`Subs::close`).
    fn drop(&mut self) {
        trace::pass!(Channel::Playground, "TOPICS:QUIT");
        self.tell_main(Msg::TopicsGone);
    }
}

impl App for TopicsTool {
    type Msg = TMsg;

    fn update(&mut self, msg: TMsg, ui: &mut Ui<TMsg>) {
        match msg {
            TMsg::Request(request) => self.request(ui, request),
            TMsg::Registry(registry) => self.registry = registry,
            TMsg::NoticeChanged => self.notice_changed(),
            TMsg::Close => {
                ui.close();
                return;
            }
            TMsg::QuitApp => self.tell_main(Msg::Quit),
            TMsg::Bell => self.bell(ui),
            TMsg::Drain => self.drain_more(ui),
            TMsg::Resized => self.probe_soon(),
            TMsg::ProbeDue => self.probe_now(ui),
            TMsg::SubTab(i) => self.sub_tab(i),
            TMsg::SubRow(row) => self.sub_row(row),
            TMsg::Pause => self.pause(),
            TMsg::CloseSub => self.close_sub(ui),
            TMsg::ClearEvents => self.clear_events(),
            TMsg::EventFilter(text) => self.event_filter(&text),
            TMsg::PinEvent => self.pin_event(),
            TMsg::CopyEvent => self.copy_event(ui),
            TMsg::SaveEvents => self.save_events(),
            TMsg::PubTopic(text) => self.publish.topic = text,
            TMsg::PubPayload(text) => self.publish.payload = text,
            TMsg::PubRetained(on) => self.publish.retained = on,
            TMsg::PubGo => self.publish_go(),
            TMsg::PubListen => self.publish_listen(ui),
            TMsg::NoticeDismiss => self.notice_dismiss(),
            TMsg::NoticeShow => self.notice_show(),
        }
        self.after(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl() -> Modifiers {
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        }
    }

    #[test]
    fn the_monitor_answers_close_and_the_notice_and_nothing_a_typed_key_means() {
        assert!(matches!(shortcut(Key::W, ctrl()), Some(TMsg::Close)));
        assert!(shortcut(Key::W, Modifiers::default()).is_none(), "typing w is typing");
        assert!(matches!(shortcut(Key::Q, ctrl()), Some(TMsg::QuitApp)), "Ctrl+Q quits the tool");
        assert!(shortcut(Key::Q, Modifiers::default()).is_none());
        let shift = Modifiers {
            shift: true,
            ..ctrl()
        };
        assert!(matches!(shortcut(Key::D, shift), Some(TMsg::NoticeShow)));
        assert!(shortcut(Key::D, ctrl()).is_none());
    }
}
