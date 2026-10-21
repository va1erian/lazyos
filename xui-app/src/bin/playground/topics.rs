//! Listening, in the Topic monitor window ([`crate::topicswin`]): native
//! subscriptions, the decoded event list, the selected event's details and
//! the Publish form (`docs/messenger-playground-plan.md` D5 and section 3).
//!
//! Events arrive by doorbell: [`TopicsTool::bell`] only reads the local bell
//! channels (never the broker) and drains the subscriptions they name. A
//! drain that stops at its batch is continued by a one-shot timer, not by
//! waiting for a ring that will not come. The broker is called only by an
//! explicit user action (Stats on Pause or tab change, Listen, Stop).

use xui_app::trace::{self, Budget, Channel};
use xui_core::app::Ui;
use xui_core::arrange::{column, Handle};
use xui_core::widget::HasText;
use xui_playground::listener::{Config, Event};

use crate::app::SubPage;
use crate::tmsg::{Request, TMsg};
use crate::topicsui::{WELCOME_TAB, WELCOME_TEXT};
use crate::topicsview::{details_rows, event_page, row_of, short_filter, stats_text};
use crate::topicswin::TopicsTool;

/// How often the doorbells are looked at while something is listened to.
const BELL_MS: u32 = 100;
/// How long before a drain that stopped at its batch continues.
const DRAIN_MS: u32 = 20;
/// Events a subscription may take before it is closed: `messengerd` never frees a
/// receive buffer, so a listen that runs for ever would use its memory up
/// (docs/messenger-playground-plan.md section 7, risk 2).
const LISTEN_BUDGET: u64 = 20_000;
/// Event lines one tab may trace per [`TRACE_WINDOW_TICKS`] (a firehose must
/// not flood the serial line): the tab's own `trace::Budget`.
pub const TRACE_LINES: u32 = 16;
/// The trace window, in PIT ticks (10 s).
pub const TRACE_WINDOW_TICKS: u64 = 1000;

impl TopicsTool {
    /// What the main window asked for.
    pub fn request(&mut self, ui: &mut Ui<TMsg>, request: Request) {
        match request {
            Request::Listen(filter) => self.listen(ui, &filter),
            Request::FillPublish { topic, payload } => self.fill_publish(&topic, &payload),
            Request::Show => {}
        }
    }

    // ---- listening -------------------------------------------------------

    pub fn listen(&mut self, ui: &mut Ui<TMsg>, filter: &str) {
        let mut config = Config::new(filter);
        config.qos = 1;
        match self.subs.listen(config) {
            Ok(index) => {
                trace::line!(Channel::Playground, "LISTEN:{filter}");
                self.add_sub_page(filter);
                self.current_sub = index;
                self.w.subs.get().select(index);
                // Start the doorbell look-out; replay arrives with the first ring
                // or right now when the retained value is already queued.
                self.start_bell(ui);
                self.drain_sub(index);
                self.subs.read_stats(index);
                self.show_stats();
                self.show_details();
                self.note(&format!("Listening to {filter}"));
                self.update_status();
            }
            Err(e) => self.note(&format!("cannot listen to {filter}: {e}")),
        }
    }

    fn add_sub_page(&mut self, filter: &str) {
        let handle: Handle<xui_core::widget::ListView<TMsg>> = Handle::new();
        let page = event_page(&handle);
        let tabs = self.w.subs.get();
        let title = short_filter(filter);
        if tabs.add_layout_page(&title, page).is_ok() {
            if self.sub_welcome {
                tabs.remove_page(0);
                self.sub_welcome = false;
            }
            self.sub_pages.push(SubPage {
                list: handle,
                selected: None,
                traced: 0,
                budget: Budget::new(TRACE_LINES, TRACE_WINDOW_TICKS),
            });
        }
        self.probe_soon();
    }

    fn start_bell(&mut self, ui: &mut Ui<TMsg>) {
        if self.timers.bell.get().is_none() {
            self.timers.bell.set(Some(ui.set_timer(BELL_MS)));
        }
    }

    fn stop_bell_if_idle(&mut self, ui: &mut Ui<TMsg>) {
        if !self.subs.is_listening() {
            if let Some(id) = self.timers.bell.take() {
                ui.kill_timer(id);
            }
        }
    }

    /// A bell timer tick: read the local doorbells and drain what rang.
    pub fn bell(&mut self, ui: &mut Ui<TMsg>) {
        for index in self.subs.ready() {
            self.drain_sub(index);
        }
        if self.subs.subs.iter().any(|s| s.more) && self.timers.drain.get().is_none() {
            self.timers.drain.set(Some(ui.set_timer(DRAIN_MS)));
        }
        self.enforce_budget(ui);
    }

    /// Close a subscription that has taken [`LISTEN_BUDGET`] events, and say so.
    fn enforce_budget(&mut self, ui: &mut Ui<TMsg>) {
        let spent = self
            .subs
            .subs
            .iter()
            .position(|s| s.listener.received() >= LISTEN_BUDGET);
        if let Some(index) = spent {
            let filter = self.subs.subs[index].listener.config().filter.clone();
            self.current_sub = index;
            self.close_sub(ui);
            self.note(&format!(
                "Stopped listening to {filter} after {LISTEN_BUDGET} events (the broker keeps a buffer per call). Listen again to continue."
            ));
        }
    }

    /// The continuation timer: drain the subscriptions that stopped at a batch.
    pub fn drain_more(&mut self, ui: &mut Ui<TMsg>) {
        if let Some(id) = self.timers.drain.take() {
            ui.kill_timer(id);
        }
        let more: Vec<usize> = self
            .subs
            .subs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.more)
            .map(|(i, _)| i)
            .collect();
        for index in more {
            self.drain_sub(index);
        }
        if self.subs.subs.iter().any(|s| s.more) {
            self.timers.drain.set(Some(ui.set_timer(DRAIN_MS)));
        }
    }

    fn drain_sub(&mut self, index: usize) {
        let now = self.now_ms();
        let registry = self.registry.clone();
        if self.subs.drain(index, now, &registry) > 0 {
            self.trace_events(index);
            self.refresh_sub_list(index);
        }
    }

    fn trace_events(&mut self, index: usize) {
        let Some(page) = self.sub_pages.get_mut(index) else {
            return;
        };
        let Some(sub) = self.subs.subs.get(index) else {
            return;
        };
        let mut newest = page.traced;
        for event in sub.listener.visible().filter(|e| e.id > page.traced) {
            newest = newest.max(event.id);
            if trace::within(&page.budget, Channel::Playground) {
                trace::line!(Channel::Playground, "EVENT:{}:{}", event.topic, event.sequence);
            }
        }
        page.traced = newest;
    }

    fn event_rows(sub: &crate::listen::Sub) -> Vec<Vec<String>> {
        // Pinned events first (they survive Clear), marked `pin` in the time column.
        sub.listener
            .pinned()
            .iter()
            .map(|e| {
                let mut row = row_of(e);
                row[0] = "pin".to_string();
                row
            })
            .chain(sub.listener.visible().map(row_of))
            .collect()
    }

    pub fn refresh_sub_list(&mut self, index: usize) {
        let (Some(sub), Some(page)) = (self.subs.subs.get(index), self.sub_pages.get(index)) else {
            return;
        };
        let rows = Self::event_rows(sub);
        let last = rows.len().saturating_sub(1);
        let paused = sub.listener.paused();
        let list = page.list.get();
        list.refresh_model(rows);
        if !paused {
            list.ensure_visible(last);
        }
        if index == self.current_sub {
            self.show_stats();
        }
    }

    // ---- the tab's controls ---------------------------------------------

    pub fn sub_tab(&mut self, index: usize) {
        if index >= self.subs.subs.len() {
            return;
        }
        self.current_sub = index;
        self.subs.read_stats(index);
        self.show_stats();
        self.show_details();
        self.probe_soon();
    }

    pub fn sub_row(&mut self, row: usize) {
        let index = self.current_sub;
        let id = self.subs.subs.get(index).and_then(|s| {
            let pinned = s.listener.pinned();
            match row.checked_sub(pinned.len()) {
                None => pinned.get(row).map(|e| e.id),
                Some(at) => s.listener.visible().nth(at).map(|e| e.id),
            }
        });
        if let Some(page) = self.sub_pages.get_mut(index) {
            page.selected = id;
        }
        self.show_details();
    }

    fn selected_event(&self) -> Option<&Event> {
        let sub = self.subs.subs.get(self.current_sub)?;
        let id = self.sub_pages.get(self.current_sub)?.selected?;
        sub.listener
            .visible()
            .find(|e| e.id == id)
            .or_else(|| sub.listener.pinned().iter().find(|e| e.id == id))
    }

    /// The selected event's metadata and value tree, in the details list
    /// (empty when nothing is selected).
    fn show_details(&mut self) {
        let rows = self.selected_event().map(details_rows).unwrap_or_default();
        self.w.details.get().refresh_model(rows);
    }

    pub fn show_stats(&self) {
        let text = match self.subs.subs.get(self.current_sub) {
            Some(sub) => {
                let s = sub.listener.strip(sub.stats);
                stats_text(&s)
            }
            None => String::new(),
        };
        self.w.sub_stats.get().set_text(&text);
    }

    pub fn pause(&mut self) {
        let index = self.current_sub;
        let Some(sub) = self.subs.subs.get_mut(index) else {
            return;
        };
        if sub.listener.paused() {
            sub.listener.resume();
        } else {
            sub.listener.pause();
        }
        let label = if sub.listener.paused() { "Resume" } else { "Pause" };
        self.w.pause.get().set_text(label);
        self.subs.read_stats(index);
        self.refresh_sub_list(index);
    }

    pub fn clear_events(&mut self) {
        let index = self.current_sub;
        if let Some(sub) = self.subs.subs.get_mut(index) {
            sub.listener.clear();
        }
        self.refresh_sub_list(index);
        self.show_details();
    }

    pub fn event_filter(&mut self, text: &str) {
        let index = self.current_sub;
        if let Some(sub) = self.subs.subs.get_mut(index) {
            sub.listener.set_filter(text);
        }
        self.refresh_sub_list(index);
    }

    pub fn pin_event(&mut self) {
        let Some(id) = self.sub_pages.get(self.current_sub).and_then(|p| p.selected) else {
            self.note("Select an event first.");
            return;
        };
        if let Some(sub) = self.subs.subs.get_mut(self.current_sub) {
            let note = if sub.listener.is_pinned(id) {
                sub.listener.unpin(id);
                "Unpinned."
            } else if sub.listener.pin(id) {
                "Pinned: it stays through Clear."
            } else {
                "Cannot pin (the pin list is full)."
            };
            self.note(note);
        }
        self.show_stats();
    }

    pub fn copy_event(&mut self, ui: &mut Ui<TMsg>) {
        match self.selected_event().map(Event::to_rhai) {
            Some(text) => {
                ui.set_clipboard_text(&text);
                self.note("Copied the event as a Rhai map.");
            }
            None => self.note("Select an event first."),
        }
    }

    pub fn save_events(&mut self) {
        let Some(sub) = self.subs.subs.get(self.current_sub) else {
            return;
        };
        let mut out = String::new();
        for e in sub.listener.pinned().iter().chain(sub.listener.visible()) {
            out.push_str(&format!(
                "{} #{} {}\n{}\n\n",
                e.topic,
                e.sequence,
                e.publisher_text(),
                e.tree()
            ));
        }
        let dir = crate::files::scripts_dir();
        let path = dir.join(format!("events-{}.txt", self.current_sub + 1));
        match std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, out)) {
            Ok(()) => self.note(&format!("Saved {}", path.display())),
            Err(e) => self.note(&format!("cannot save {}: {e}", path.display())),
        }
    }

    /// Stop listening on the current tab.
    pub fn close_sub(&mut self, ui: &mut Ui<TMsg>) {
        let index = self.current_sub;
        if index >= self.subs.subs.len() {
            return;
        }
        let refused = self.subs.close(index);
        let tabs = self.w.subs.get();
        if self.sub_pages.len() > index {
            self.sub_pages.remove(index);
            tabs.remove_page(index);
        }
        if self.sub_pages.is_empty() {
            let _ = tabs.add_layout_page(
                WELCOME_TAB,
                column().child(xui_core::arrange::label(WELCOME_TEXT)),
            );
            self.sub_welcome = true;
        }
        self.current_sub = self.current_sub.min(self.sub_pages.len().saturating_sub(1));
        self.stop_bell_if_idle(ui);
        self.show_stats();
        self.show_details();
        self.update_status();
        self.probe_soon();
        if let Some(error) = refused {
            self.note(&format!("The broker did not drop the subscription ({error}); it is tried again on the next close."));
        }
    }
}
