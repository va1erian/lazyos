//! `sysmon`: a windowed dashboard over the native system-stats snapshot
//! (syscall 14, issue #144).
//!
//! The whole window is one owner-drawn node: a header, three memory gauges
//! (frames, slab, kernel heap), the task table (pid, state, class, CPU ticks,
//! name) and a footer with uptime. A one-second `ui` timer refreshes the
//! snapshot; `r` refreshes immediately and `q` quits. Text uses the bundled
//! Droid Sans through the backend's font.
//!
//! Serial evidence: `SYSMON:UP:PASS` after the first frame (or
//! `SYSMON:UP:FAIL:<errno>` when the snapshot is unreadable),
//! `SYSMON:REFRESH:PASS` on `r`, `SYSMON:QUIT:PASS` on `q` (or the window close button), and a
//! `SYSMON:DATA:...` line with the headline counters.

use std::cell::RefCell;
use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::sysinfo::{self, Snapshot};
use xui_core::app::{run_app, App, Ui};
use xui_core::backend::{Backend, Event, NodeKind, NodeSpec, PlatformSpec};
use xui_core::Control;

#[path = "sysmon/render.rs"]
mod render;

use render::paint;

/// The window size when a compositor lays the app out (issue #215); as the
/// display owner it fills the screen instead.
const WINDOW: (i32, i32) = (860, 600);

/// The memory-card height when the window is tall enough: four value lines.
const CARD_H: i32 = 164;
/// The smallest the cards shrink to before their value lines would clip.
const CARD_MIN_H: i32 = 158;
/// Horizontal gap between the three memory cards.
const CARD_GAP: i32 = 16;
/// Vertical gap between the memory cards and the task table.
const TABLE_GAP: i32 = 20;
/// Height reserved for the footer line.
const FOOTER_H: i32 = 26;

/// How often the snapshot refreshes.
const REFRESH_MILLIS: u32 = 1000;

/// One application message.
enum Msg {
    /// The one-second timer fired.
    Tick,
    /// The user pressed `r`.
    Refresh,
    /// The user pressed `q`.
    Quit,
}

/// The decoded snapshot and the last error, shared with the painter.
struct State {
    snapshot: Option<Snapshot>,
    error: Option<i64>,
    refreshes: u64,
}

impl State {
    /// Read the snapshot now.
    fn load() -> State {
        let mut state = State {
            snapshot: None,
            error: None,
            refreshes: 0,
        };
        state.reload();
        state
    }

    /// Replace the snapshot; on failure keep the previous one and record the
    /// errno so the page can show it.
    fn reload(&mut self) {
        match sysinfo::snapshot() {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.error = None;
            }
            Err(code) => self.error = Some(code),
        }
    }
}

/// The dashboard app: one owner-drawn node plus its state.
struct Sysmon {
    state: Rc<RefCell<State>>,
    root: Control<Msg>,
}

impl App for Sysmon {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick | Msg::Refresh => {
                let mut state = self.state.borrow_mut();
                state.reload();
                state.refreshes += 1;
                let error = state.error;
                drop(state);
                ui.invalidate(self.root.id());
                if matches!(msg, Msg::Refresh) {
                    println!("SYSMON:REFRESH:PASS");
                }
                if let Some(code) = error {
                    println!("SYSMON:SNAPSHOT:FAIL:{code}");
                }
            }
            Msg::Quit => {
                println!("SYSMON:QUIT:PASS");
                ui.quit();
            }
        }
    }
}

fn main() {
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("SYSMON:BIND:FAIL:{code}");
            std::process::exit(1);
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    let state = Rc::new(RefCell::new(State::load()));

    {
        let state = Rc::clone(&state);
        backend.on_first_frame(move || {
            let state = state.borrow();
            match state.snapshot {
                Some(snapshot) => {
                    println!("SYSMON:UP:PASS");
                    println!(
                        "SYSMON:DATA:tasks={} frames_free={} frames_live={} slab_live={} heap_used={}",
                        snapshot.tasks_live,
                        snapshot.frames_free,
                        snapshot.frames_live,
                        snapshot.slab_live,
                        snapshot.heap_used
                    );
                }
                None => println!("SYSMON:UP:FAIL:{}", state.error.unwrap_or(-22)),
            }
        });
    }

    let spec =
        PlatformSpec::new("sysmon").size(xui_core::Dip(width as f32), xui_core::Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, |ui| {
        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
            .expect("root node");
        {
            let state = Rc::clone(&state);
            root.set_painter(Rc::new(move |canvas| paint(canvas, &state.borrow())));
        }
        root.on_events(|event| match event {
            Event::Char('r') => Some(Msg::Refresh),
            Event::Char('q') => Some(Msg::Quit),
            _ => None,
        });
        root.focus();
        ui.on_timer(|_| Some(Msg::Tick));
        ui.on_close(|| Some(Msg::Quit));
        ui.set_timer(REFRESH_MILLIS);
        Sysmon { state, root }
    });

    backend.unbind();
    match outcome {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            println!("SYSMON:RUN:FAIL:{error}");
            std::process::exit(1);
        }
    }
}
