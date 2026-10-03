//! `fabricmon`: a windowed Messenger fabric panel.
//!
//! Three read-only syscall-5 views, refreshed on a one-second timer:
//! the global version-2 `FabricStats` snapshot (`stats`), the kernel name
//! registry (`list`, names with owners and interfaces) and the userspace
//! topics broker's `list_topics` reply (topic/subscription counts). `r`
//! refreshes now and `q` quits.
//!
//! Serial evidence: `FABMON:UP:PASS` after the first frame (or
//! `FABMON:UP:FAIL:<errno>` when the fabric snapshot is unreadable),
//! `FABMON:READY:PASS` once the event loop has ticked (input is now handled),
//! `FABMON:REFRESH:PASS` on `r`, `FABMON:QUIT:PASS` on `q`, and a
//! `FABMON:DATA:...` line with the headline counters. `c` (or the chip)
//! toggles the compact view by asking the compositor for a size, and
//! `FABMON:SIZE:<w>x<h>` follows every resize.

use std::cell::RefCell;
use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::compact;
use xui_app::fabric::{self, FabricStats, RegistryEntry, Topics};
use xui_core::app::{run_app, App, Ui};
use xui_core::backend::{Backend, Event, NodeKind, NodeSpec, PlatformSpec, WidgetId};
use xui_core::{Control, MouseButton};

#[path = "fabricmon/compact.rs"]
mod compact_view;

#[path = "fabricmon/render.rs"]
mod render;

/// The window size when a compositor lays the app out (issue #215); as the
/// display owner it fills the screen instead.
const WINDOW: (i32, i32) = (900, 600);

/// How often the fabric snapshot refreshes.
const REFRESH_MILLIS: u32 = 1000;

/// One application message.
enum Msg {
    /// The one-second timer fired.
    Tick,
    /// The user pressed `r`.
    Refresh,
    /// The user pressed `q`.
    Quit,
    /// The user pressed `c` or clicked the chip.
    ToggleCompact,
    /// The compositor resized the window.
    Resized,
}

/// Everything the panel renders, shared with the painter.
struct State {
    stats: Option<FabricStats>,
    stats_error: Option<i64>,
    registry: Option<Vec<RegistryEntry>>,
    registry_error: Option<i64>,
    topics: Option<Topics>,
    /// The cached broker endpoint; see [`fabric::Broker`].
    broker: fabric::Broker,
    /// Timer ticks since the app started, for the broker poll cadence.
    tick: u64,
    refreshes: u64,
}

/// Every Nth timer tick also polls the topics broker. The broker's bump heap
/// never frees a receive buffer, so a per-second call would exhaust
/// `messengerd` after ~100 polls; the registry and stats are kernel reads and
/// refresh every tick.
const TOPICS_EVERY: u64 = 10;

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
            tick: 0,
            refreshes: 0,
        };
        state.reload(true);
        state
    }

    /// Refresh the fabric snapshot and registry every tick; refresh the topics
    /// broker too on the first load, on `manual` (`r`), and every
    /// [`TOPICS_EVERY`] ticks.
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
        self.tick += 1;
        if manual || self.tick.is_multiple_of(TOPICS_EVERY) {
            self.topics = Some(self.broker.topics());
        }
    }
}

/// The fabric panel app: one owner-drawn node plus its state.
struct Fabricmon {
    state: Rc<RefCell<State>>,
    root: Control<Msg>,
    backend: Rc<LazyOSBackend>,
    /// The last full-size window, restored when leaving compact mode.
    full_size: (i32, i32),
}

impl App for Fabricmon {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick | Msg::Refresh => {
                let mut state = self.state.borrow_mut();
                state.reload(matches!(msg, Msg::Refresh));
                state.refreshes += 1;
                // The first timer tick proves the event loop is dispatching, so
                // scripted input sent after this marker cannot be lost.
                let first_tick = matches!(msg, Msg::Tick) && state.refreshes == 1;
                drop(state);
                if first_tick {
                    println!("FABMON:READY:PASS");
                }
                ui.invalidate(self.root.id());
                if matches!(msg, Msg::Refresh) {
                    println!("FABMON:REFRESH:PASS");
                }
            }
            Msg::Quit => {
                println!("FABMON:QUIT:PASS");
                ui.quit();
            }
            Msg::ToggleCompact => {
                let rect = xui_app::hidpi::design_rect(ui);
                let (w, h) = compact::toggle_target(
                    (rect.width(), rect.height()),
                    self.full_size,
                    (WINDOW.0 as u32, WINDOW.1 as u32),
                );
                self.backend.request_size(w, h);
            }
            Msg::Resized => {
                let rect = ui.client_rect();
                let design = xui_app::hidpi::design_rect(ui);
                if !compact::is_compact(design.width(), design.height()) {
                    self.full_size = (design.width(), design.height());
                }
                ui.apply_moves(&[(self.root.id(), rect)]);
                ui.invalidate(self.root.id());
                println!("FABMON:SIZE:{}x{}", rect.width(), rect.height());
            }
        }
    }
}

fn main() {
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("FABMON:BIND:FAIL:{code}");
            std::process::exit(1);
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    let state = Rc::new(RefCell::new(State::load()));
    backend.set_size_hints(compact::MIN_SIZE.0, compact::MIN_SIZE.1, 0, 0);

    {
        let state = Rc::clone(&state);
        backend.on_first_frame(move || {
            let state = state.borrow();
            match &state.stats {
                Some(stats) => {
                    println!("FABMON:UP:PASS");
                    let names = state.registry.as_ref().map_or(0, |entries| entries.len());
                    let topics = match &state.topics {
                        Some(Topics::Online(topics)) => topics.len(),
                        _ => 0,
                    };
                    println!(
                        "FABMON:DATA:channels={} endpoints={} buffers={} fences_submitted={} names={} topics={}",
                        stats.channels,
                        stats.endpoints,
                        stats.buffers,
                        stats.fences_submitted,
                        names,
                        topics
                    );
                }
                None => println!("FABMON:UP:FAIL:{}", state.stats_error.unwrap_or(-22)),
            }
        });
    }

    let spec = PlatformSpec::new("fabricmon")
        .size(xui_core::Dip(width as f32), xui_core::Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, |ui| {
        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
            .expect("root node");
        {
            let state = Rc::clone(&state);
            root.set_painter(Rc::new(move |canvas| {
                render::paint(canvas, &state.borrow())
            }));
        }
        let ui_probe = ui.clone();
        root.on_events(move |event| match event {
            Event::Char('r') => Some(Msg::Refresh),
            Event::Char('q') => Some(Msg::Quit),
            Event::Char('c') => Some(Msg::ToggleCompact),
            Event::MouseDown {
                x,
                y,
                button: MouseButton::Left,
                ..
            } if {
                let (x, y) = xui_app::hidpi::design_point(&ui_probe, *x, *y);
                compact::hit_chip(xui_app::hidpi::design_rect(&ui_probe), x, y)
            } =>
            {
                Some(Msg::ToggleCompact)
            }
            _ => None,
        });
        ui.register_events(WidgetId::NONE, |event| match event {
            Event::Resize { .. } => Some(Msg::Resized),
            _ => None,
        });
        root.focus();
        ui.on_timer(|_| Some(Msg::Tick));
        ui.on_close(|| Some(Msg::Quit));
        ui.set_timer(REFRESH_MILLIS);
        Fabricmon {
            state,
            root,
            backend: Rc::clone(&backend),
            full_size: WINDOW,
        }
    });

    backend.unbind();
    match outcome {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            println!("FABMON:RUN:FAIL:{error}");
            std::process::exit(1);
        }
    }
}
