//! `fabricmon`: a windowed Messenger fabric panel.
//!
//! Three read-only syscall-5 views, refreshed on a one-second timer: the
//! global version-2 `FabricStats` snapshot (`stats`), the kernel name
//! registry (`list`, names with owners and interfaces) and the userspace
//! topics broker's `list_topics` reply (topic/subscription counts). They are
//! standard xui widgets laid out without coordinates, on two tabs: Fabric
//! (the counters and per-task usage) and Names & topics. `f`/`n` (or a click
//! on a tab) switch tabs, `r` refreshes now, `c` toggles the compact view
//! (four headline counters) by asking the compositor for a size, and `q`
//! quits.
//!
//! Serial evidence: `FABMON:UP:PASS` after the first frame (or
//! `FABMON:UP:FAIL:<errno>` when the fabric snapshot is unreadable),
//! `FABMON:READY:PASS` once the event loop has ticked (input is now handled),
//! `FABMON:REFRESH:PASS` on `r`, `FABMON:QUIT:PASS` on `q` (or the window
//! close button), a `FABMON:DATA:...` line with the headline counters, and
//! `FABMON:SIZE:<w>x<h>` after every resize.

use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::fabric::{self, FabricStats, RegistryEntry, Topics};
use xui_app::{compact, hidpi, launch};
use xui_core::app::{App, Ui};
use xui_core::backend::{Event, WidgetId};
use xui_core::Key;

#[path = "fabricmon/view.rs"]
mod view;

use view::Widgets;

/// The window size when a compositor lays the app out (issue #215); as the
/// display owner it fills the screen instead.
const WINDOW: (i32, i32) = (900, 600);
/// How often the fabric snapshot refreshes.
const REFRESH_MILLIS: u32 = 1000;
/// Every Nth timer tick also polls the topics broker. The broker's bump heap
/// never frees a receive buffer, so a per-second call would exhaust
/// `messengerd` after ~100 polls; the registry and stats are kernel reads and
/// refresh every tick.
const TOPICS_EVERY: u64 = 10;

/// One application message.
#[derive(Clone)]
pub(crate) enum Msg {
    Tick,
    Refresh,
    Quit,
    ToggleCompact,
    Resized,
    /// Show the tab at this index.
    Show(usize),
}

/// The last readings of each view; a failed read keeps the previous value
/// and records its errno.
pub(crate) struct State {
    pub(crate) stats: Option<FabricStats>,
    pub(crate) stats_error: Option<i64>,
    pub(crate) registry: Option<Vec<RegistryEntry>>,
    pub(crate) registry_error: Option<i64>,
    pub(crate) topics: Option<Topics>,
    /// The cached broker endpoint; see [`fabric::Broker`].
    broker: fabric::Broker,
    /// Reads since the app started, for the broker poll cadence.
    reads: u64,
}

impl State {
    /// Read every view now.
    fn load() -> State {
        let mut state = State {
            stats: None,
            stats_error: None,
            registry: None,
            registry_error: None,
            topics: None,
            broker: fabric::Broker::new(),
            reads: 0,
        };
        state.reload(true);
        state
    }

    /// Refresh the fabric snapshot and registry every time; refresh the topics
    /// broker too on the first load, on `manual` (`r`), and every
    /// [`TOPICS_EVERY`] reads.
    fn reload(&mut self, manual: bool) {
        match fabric::fabric_stats() {
            Ok(stats) => {
                self.stats = Some(stats);
                self.stats_error = None;
            }
            Err(code) => self.stats_error = Some(code),
        }
        match fabric::registry() {
            Ok(entries) => {
                self.registry = Some(entries);
                self.registry_error = None;
            }
            Err(code) => self.registry_error = Some(code),
        }
        self.reads += 1;
        if manual || self.reads.is_multiple_of(TOPICS_EVERY) {
            self.topics = Some(self.broker.topics());
        }
    }

    /// Registered names, zero while the registry is unreadable.
    pub(crate) fn names(&self) -> usize {
        self.registry.as_ref().map_or(0, Vec::len)
    }

    /// Topics the broker reported, zero while it is offline.
    pub(crate) fn topic_count(&self) -> usize {
        match &self.topics {
            Some(Topics::Online(topics)) => topics.len(),
            _ => 0,
        }
    }

    /// The first-frame evidence for the readings taken at start-up.
    fn evidence(&self) -> String {
        match &self.stats {
            Some(s) => format!(
                "FABMON:UP:PASS\nFABMON:DATA:channels={} endpoints={} buffers={} handoffs={} names={} topics={}",
                s.channels,
                s.endpoints,
                s.buffers,
                s.handoffs,
                self.names(),
                self.topic_count()
            ),
            None => format!("FABMON:UP:FAIL:{}", self.stats_error.unwrap_or(-22)),
        }
    }
}

struct Fabricmon {
    widgets: Widgets,
    backend: Rc<LazyOSBackend>,
    state: State,
    refreshes: u64,
    /// Whether the timer has fired yet: the first tick proves the event loop
    /// is dispatching, so scripted input sent after `READY` cannot be lost.
    ticked: bool,
    /// The last full-size window, restored when leaving compact mode.
    full_size: (i32, i32),
}

impl App for Fabricmon {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick | Msg::Refresh => {
                let manual = matches!(msg, Msg::Refresh);
                self.state.reload(manual);
                self.refreshes += 1;
                self.widgets.show(&self.state, self.refreshes);
                if !manual && !self.ticked {
                    self.ticked = true;
                    println!("FABMON:READY:PASS");
                }
                if manual {
                    println!("FABMON:REFRESH:PASS");
                }
            }
            Msg::Show(index) => self.widgets.tabs.get().select(index),
            Msg::Quit => {
                println!("FABMON:QUIT:PASS");
                ui.quit();
            }
            Msg::ToggleCompact => {
                let rect = hidpi::design_rect(ui);
                let (w, h) = compact::toggle_target(
                    (rect.width(), rect.height()),
                    self.full_size,
                    (WINDOW.0 as u32, WINDOW.1 as u32),
                );
                self.backend.request_size(w, h);
            }
            Msg::Resized => {
                let design = hidpi::design_rect(ui);
                let small = compact::is_compact(design.width(), design.height());
                if !small {
                    self.full_size = (design.width(), design.height());
                }
                self.widgets.set_compact(ui, small);
                let rect = ui.client_rect();
                println!("FABMON:SIZE:{}x{}", rect.width(), rect.height());
            }
        }
        // Longer values change the labels' natural sizes.
        ui.relayout();
    }
}

/// The keys the panel answers wherever the focus is.
fn shortcut(key: Key) -> Option<Msg> {
    Some(match key {
        Key::R => Msg::Refresh,
        Key::Q => Msg::Quit,
        Key::C => Msg::ToggleCompact,
        Key::F => Msg::Show(0),
        Key::N => Msg::Show(1),
        _ => return None,
    })
}

fn main() {
    launch::run("FABMON", "fabricmon", WINDOW, |ui, backend| {
        // Resizable, down to the compact view, so `RequestSize` is accepted.
        backend.set_size_hints(compact::MIN_SIZE.0, compact::MIN_SIZE.1, 0, 0);
        let state = State::load();
        let first = state.evidence();
        backend.on_first_frame(move || println!("{first}"));

        let widgets = Widgets::default();
        ui.root(widgets.layout())?;
        ui.on_key(|key, _| shortcut(key));
        ui.on_close(|| Some(Msg::Quit));
        ui.register_events(WidgetId::NONE, |event| {
            matches!(event, Event::Resize { .. }).then_some(Msg::Resized)
        });
        ui.every(REFRESH_MILLIS, Msg::Tick);

        widgets.show(&state, 0);
        let design = hidpi::design_rect(ui);
        widgets.set_compact(ui, compact::is_compact(design.width(), design.height()));
        Ok(Fabricmon {
            widgets,
            backend: Rc::clone(backend),
            state,
            refreshes: 0,
            ticked: false,
            full_size: WINDOW,
        })
    })
}
