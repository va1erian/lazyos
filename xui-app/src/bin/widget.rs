//! `widget`: a tiny always-glanceable "CPU & Memory" window.
//!
//! Two bars refreshed on a one-second timer from the system snapshot
//! (syscall 14): CPU load between consecutive samples
//! ([`sysinfo::cpu_percent`]) and physical frames in use. `q` quits.
//!
//! Serial evidence: `WIDGET:UP:PASS` after the first frame (or
//! `WIDGET:UP:FAIL:<errno>`), `WIDGET:TICK:cpu=<pct> mem=<pct>` on every
//! refresh, `WIDGET:QUIT:PASS` on `q` or the close button.

use std::cell::RefCell;
use std::rc::Rc;

use xui_app::dashboard as dash;
use xui_app::launch;
use xui_app::sysinfo::{self, CpuSample};
use xui_core::app::{App, Ui};
use xui_core::backend::{Event, NodeKind, NodeSpec};
use xui_core::theme::look;
use xui_core::{Canvas, Control, Rect, Theme};

/// The window size when a compositor lays the app out.
const WINDOW: (i32, i32) = (200, 90);

/// How often the bars refresh.
const REFRESH_MILLIS: u32 = 1000;

/// One application message.
#[derive(Clone)]
enum Msg {
    Tick,
    Quit,
}

/// The latest readings, shared with the painter.
struct State {
    /// CPU load, 0..=100.
    cpu: u32,
    /// Physical memory in use, 0..=100.
    mem: u32,
    /// The previous CPU sample, the baseline for the next interval.
    last: Option<CpuSample>,
    /// The errno of the last failed snapshot.
    error: Option<i64>,
}

impl State {
    fn load() -> State {
        let mut state = State {
            cpu: 0,
            mem: 0,
            last: None,
            error: None,
        };
        state.reload();
        state
    }

    /// Take a snapshot and update both gauges; on failure keep the old values.
    fn reload(&mut self) {
        match sysinfo::snapshot() {
            Ok(snapshot) => {
                let sample = snapshot.cpu_sample();
                if let Some(prev) = self.last {
                    self.cpu = sysinfo::cpu_percent(prev, sample);
                }
                self.last = Some(sample);
                self.mem = percent(snapshot.frames_live, snapshot.frames_total);
                self.error = None;
            }
            Err(code) => self.error = Some(code),
        }
    }
}

/// `part` as a whole percent of `total` (0 when the total is 0).
fn percent(part: u64, total: u64) -> u32 {
    if total == 0 {
        0
    } else {
        (u128::from(part) * 100 / u128::from(total)).min(100) as u32
    }
}

struct Widget {
    state: Rc<RefCell<State>>,
    root: Control<Msg>,
}

impl App for Widget {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick => {
                let mut state = self.state.borrow_mut();
                state.reload();
                println!("WIDGET:TICK:cpu={} mem={}", state.cpu, state.mem);
                drop(state);
                ui.invalidate(self.root.id());
            }
            Msg::Quit => {
                println!("WIDGET:QUIT:PASS");
                ui.quit();
            }
        }
    }
}

/// Paint the title and the two labelled bars.
fn paint(canvas: &mut dyn Canvas, theme: Theme, state: &State) {
    let bounds = xui_app::hidpi::design_bounds(canvas);
    look::paint_background(canvas, bounds, bounds, &theme);
    canvas.draw_text(
        "CPU & Memory",
        Rect::new(
            bounds.left + 10,
            bounds.top + 4,
            bounds.right - 10,
            bounds.top + 24,
        ),
        &dash::heading(theme.text, dash::SECTION),
    );
    if let Some(code) = state.error {
        canvas.draw_text(
            &format!("snapshot unavailable ({code})"),
            Rect::new(
                bounds.left + 10,
                bounds.top + 30,
                bounds.right - 10,
                bounds.bottom,
            ),
            &dash::heading(theme.danger, dash::LABEL),
        );
        return;
    }
    for (index, (label, value)) in [("CPU", state.cpu), ("Mem", state.mem)]
        .into_iter()
        .enumerate()
    {
        let top = bounds.top + 30 + index as i32 * 26;
        let row = Rect::new(bounds.left + 10, top, bounds.right - 10, top + 22);
        canvas.draw_text(
            label,
            Rect::new(row.left, row.top, row.left + 30, row.bottom),
            &dash::heading(theme.text_secondary, dash::LABEL),
        );
        let bar = Rect::new(row.left + 34, top + 6, row.right - 42, top + 16);
        let fraction = f64::from(value) / 100.0;
        let color = if value >= 90 {
            theme.danger
        } else if value >= 75 {
            theme.warning
        } else {
            theme.accent
        };
        dash::bar(canvas, theme, bar, fraction, color);
        canvas.draw_text(
            &format!("{value}%"),
            Rect::new(bar.right + 4, row.top, row.right, row.bottom),
            &dash::heading_end(theme.text, dash::LABEL),
        );
    }
}

fn main() {
    launch::run("WIDGET", "widget", WINDOW, |ui, backend| {
        let state = Rc::new(RefCell::new(State::load()));
        {
            let state = Rc::clone(&state);
            backend.on_first_frame(move || match state.borrow().error {
                None => println!("WIDGET:UP:PASS"),
                Some(code) => println!("WIDGET:UP:FAIL:{code}"),
            });
        }

        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))?;
        {
            let state = Rc::clone(&state);
            let theme = ui.theme_handle();
            root.set_painter(Rc::new(move |canvas| {
                paint(canvas, theme.get(), &state.borrow())
            }));
        }
        root.on_events(|event| match event {
            Event::Char('q') => Some(Msg::Quit),
            _ => None,
        });
        root.focus();
        ui.every(REFRESH_MILLIS, Msg::Tick);
        ui.on_close(|| Some(Msg::Quit));
        Ok(Widget { state, root })
    })
}
