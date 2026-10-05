//! The `sysmon` window's widgets: the layout, and how each refresh fills it.
//!
//! Everything is a standard xui widget placed by a layout: the memory gauges
//! are group boxes with a progress bar over a key/value grid, the task and
//! service tables are list views, and the footer is a status bar. Resizing,
//! HiDPI and the theme come from the layout and the widgets.

use xui_app::format::{bytes, uptime};
use xui_app::services::Services;
use xui_app::sysinfo::{Snapshot, MAX_TASKS};
use xui_core::app::Ui;
use xui_core::backend::WidgetId;
use xui_core::prelude::*;

use crate::{Msg, View};

/// A gauge's progress bar counts in thousandths.
const GAUGE: i32 = 1000;
/// The keys the header reminds of.
const KEYS: &str = "1 s refresh · [r] refresh  [o] overview  [s] services  [c] compact  [q] quit";

/// A memory gauge: a bar and one value per line under it.
#[derive(Default)]
pub(crate) struct Gauge {
    bar: Handle<ProgressBar<Msg>>,
    values: Vec<Handle<Label<Msg>>>,
}

impl Gauge {
    fn new(lines: usize) -> Gauge {
        Gauge {
            bar: Handle::new(),
            values: (0..lines).map(|_| Handle::new()).collect(),
        }
    }

    /// The card titled `title`: the bar over one `key  value` row per line.
    fn card(&self, title: &str, keys: &[&str]) -> Entry<Msg> {
        let mut rows = Vec::new();
        for (key, value) in keys.iter().zip(&self.values) {
            rows.push(label(*key).into_entry());
            rows.push(label("").bind(value).into_entry());
        }
        group(
            title,
            column().gap(8).children((
                progress(GAUGE).bind(&self.bar),
                grid([Track::Auto, Track::Fill(1)]).gap(6).children(rows),
            )),
        )
        .into_entry()
    }

    fn show(&self, fraction: f64, lines: &[String]) {
        self.bar
            .get()
            .set_value((fraction * f64::from(GAUGE)).round() as i32);
        for (value, text) in self.values.iter().zip(lines) {
            value.get().set_text(text);
        }
    }
}

/// One line of the compact view: a name, a bar and a value.
#[derive(Default)]
pub(crate) struct Meter {
    name: Handle<Label<Msg>>,
    bar: Handle<ProgressBar<Msg>>,
    value: Handle<Label<Msg>>,
}

impl Meter {
    fn cells(&self, name: &str) -> Vec<Entry<Msg>> {
        vec![
            label(name).bind(&self.name).into_entry(),
            progress(GAUGE).bind(&self.bar).align(Align::Center),
            label("").bind(&self.value).into_entry(),
        ]
    }

    fn show(&self, fraction: f64, value: String) {
        self.bar
            .get()
            .set_value((fraction * f64::from(GAUGE)).round() as i32);
        self.value.get().set_text(&value);
    }

    fn ids(&self) -> [WidgetId; 3] {
        [
            self.name.get().id(),
            self.bar.get().id(),
            self.value.get().id(),
        ]
    }
}

/// Every widget the app changes after start-up.
#[derive(Default)]
pub(crate) struct Widgets {
    hint: Handle<Label<Msg>>,
    pub(crate) tabs: Handle<Tabs<Msg>>,
    frames: Gauge,
    slab: Gauge,
    heap: Gauge,
    tasks_heading: Handle<Label<Msg>>,
    pub(crate) tasks: Handle<ListView<Msg>>,
    services_heading: Handle<Label<Msg>>,
    services_note: Handle<Label<Msg>>,
    services: Handle<ListView<Msg>>,
    compact: [Meter; 3],
    pub(crate) status: Handle<StatusBar<Msg>>,
}

impl Widgets {
    pub(crate) fn new() -> Widgets {
        Widgets {
            frames: Gauge::new(4),
            slab: Gauge::new(3),
            heap: Gauge::new(3),
            ..Widgets::default()
        }
    }

    /// The whole window: a header, the tabs (or, when compact, the meters)
    /// and the status bar.
    pub(crate) fn layout(&self) -> Layout<Msg> {
        let mut meters = Vec::new();
        for (meter, name) in self.compact.iter().zip(["Frames", "Slab", "Heap"]) {
            meters.extend(meter.cells(name));
        }
        column().padding(12).gap(10).children((
            row()
                .gap(16)
                .align(Align::Center)
                .children((label("sysmon").title(), label(KEYS).bind(&self.hint))),
            tabs()
                .page("Overview", self.overview())
                .page("Services", self.services_page())
                .on_change(|index| Msg::Show(View::from_index(index)))
                .bind(&self.tabs)
                .fill(1),
            grid([Track::Auto, Track::Fill(1), Track::Auto])
                .gap(8)
                .children(meters),
            status_bar(&["", "", "", ""]).bind(&self.status),
        ))
    }

    fn overview(&self) -> Layout<Msg> {
        column().padding(8).gap(10).children((
            grid([Track::Fill(1); 3]).gap(16).children((
                self.frames
                    .card("Frames", &["live", "free", "reserved", "double frees"]),
                self.slab.card("Slab", &["live", "peak", "oversized"]),
                self.heap.card("Kernel heap", &["used", "free", "counters"]),
            )),
            label("Tasks").bind(&self.tasks_heading),
            list()
                .column_right("pid", 60)
                .column("state", 90)
                .column("class", 90)
                .column_right("cpu ticks", 120)
                .column("name", Fill)
                .bind(&self.tasks)
                .fill(1),
        ))
    }

    fn services_page(&self) -> Layout<Msg> {
        column().padding(8).gap(8).children((
            label("Loading services…").bind(&self.services_heading),
            label("").bind(&self.services_note),
            list()
                .column("service", 130)
                .column("state", 90)
                .column_right("pid", 50)
                .column_right("restarts", 70)
                .column("health", 80)
                .column("depends on", 110)
                .column("detail", Fill)
                .bind(&self.services)
                .fill(1),
        ))
    }

    /// Shows the full view, or only the header title and the meters.
    pub(crate) fn set_compact(&self, ui: &Ui<Msg>, compact: bool) {
        for meter in &self.compact {
            for id in meter.ids() {
                ui.set_visible(id, compact);
            }
        }
        for id in [
            self.hint.get().id(),
            self.tabs.get().id(),
            self.status.get().id(),
        ] {
            ui.set_visible(id, !compact);
        }
    }

    /// Fills the gauges, the task table and the status bar from `snapshot`.
    pub(crate) fn show_snapshot(&self, snapshot: &Snapshot) {
        let s = snapshot;
        let frames = share(s.frames_live, s.frames_total);
        let slab = share(s.slab_live, s.slab_peak.max(s.slab_live));
        let heap = share(s.heap_used, s.heap_total);
        self.frames.show(
            frames,
            &[
                format!(
                    "{} / {} ({}%)",
                    s.frames_live,
                    s.frames_total,
                    percent(frames)
                ),
                s.frames_free.to_string(),
                s.frames_reserved.to_string(),
                format!(
                    "{} · invalid {}",
                    s.frames_double_frees, s.frames_invalid_frees
                ),
            ],
        );
        self.slab.show(
            slab,
            &[
                bytes(s.slab_live),
                bytes(s.slab_peak),
                format!(
                    "{} · peak {}",
                    bytes(s.slab_oversized),
                    bytes(s.slab_oversized_peak)
                ),
            ],
        );
        self.heap.show(
            heap,
            &[
                format!(
                    "{} / {} ({}%)",
                    bytes(s.heap_used),
                    bytes(s.heap_total),
                    percent(heap)
                ),
                bytes(s.heap_free),
                format!("alloc {} · free {}", s.frames_allocated, s.frames_freed),
            ],
        );
        let [frames_meter, slab_meter, heap_meter] = &self.compact;
        frames_meter.show(frames, format!("{} / {}", s.frames_live, s.frames_total));
        slab_meter.show(slab, bytes(s.slab_live));
        heap_meter.show(heap, bytes(s.heap_used));

        self.tasks_heading.get().set_text(&format!(
            "Tasks — {} live of {MAX_TASKS} slots (100 Hz ticks)",
            s.tasks_live
        ));
        let rows: Vec<Vec<String>> = s
            .live_tasks()
            .map(|task| {
                vec![
                    task.pid.to_string(),
                    task.state.label().to_string(),
                    task.class.label().to_string(),
                    format!("{} ({:.1}s)", task.cpu_ticks, task.cpu_ticks as f64 / 100.0),
                    format!("{} (ppid {})", task.name(), task.ppid),
                ]
            })
            .collect();
        self.tasks.get().refresh_model(rows);
    }

    /// The status bar: uptime, tick, refreshes and the snapshot source (or the
    /// last refresh's error).
    pub(crate) fn show_status(&self, ticks: u64, refreshes: u64, error: Option<i64>) {
        let source = match error {
            Some(code) => format!("last refresh failed: errno {code}"),
            None => "snapshot v2 via syscall 14".to_string(),
        };
        let status = self.status.get();
        status.set_text(0, &format!("uptime {}", uptime(ticks)));
        status.set_text(1, &format!("tick {ticks}"));
        status.set_text(2, &format!("{refreshes} refresh(es)"));
        status.set_text(3, &source);
    }

    /// Says the snapshot cannot be read and clears the values it would show,
    /// so neither view keeps the last successful read; the compact meters
    /// (shown without the tabs or the status bar) carry the errno.
    pub(crate) fn show_unavailable(&self, code: i64) {
        self.tasks_heading.get().set_text(&format!(
            "System snapshot unavailable: syscall 14 returned errno {code}"
        ));
        for gauge in [&self.frames, &self.slab, &self.heap] {
            gauge.show(0.0, &[]);
            for value in &gauge.values {
                value.get().set_text("");
            }
        }
        for meter in &self.compact {
            meter.show(0.0, format!("errno {code}"));
        }
        self.tasks.get().refresh_model(Vec::<Vec<String>>::new());
    }

    /// Fills the Services tab. The note line is hidden while there is
    /// nothing to say, so it leaves no gap above the table.
    pub(crate) fn show_services(&self, ui: &Ui<Msg>, view: &Services) {
        let (good, warn, bad) = view.counts();
        let mut heading = format!(
            "Services — {} listed · {good} ok · {warn} degraded · {bad} down",
            view.rows.len()
        );
        if let Some(summary) = &view.summary {
            heading.push_str(&format!(" · system: {} {}", summary.status, summary.detail));
        }
        self.services_heading.get().set_text(&heading);
        let mut notes = Vec::new();
        if let Some(code) = view.init_error {
            notes.push(format!("init unavailable (errno {code})"));
        }
        if let Some(code) = view.health_error {
            notes.push(format!("healthd unavailable (errno {code})"));
        }
        if view.rows.is_empty() {
            notes
                .push("No supervised services (is the image built with LAZYOS_SERVICES=1?)".into());
        }
        let note = self.services_note.get();
        note.set_text(&notes.join(" · "));
        ui.set_visible(note.id(), !notes.is_empty());
        let rows: Vec<Vec<String>> = view
            .rows
            .iter()
            .map(|row| {
                vec![
                    row.name.clone(),
                    row.state.clone(),
                    row.pid.to_string(),
                    row.restarts.to_string(),
                    row.health.clone(),
                    row.deps.clone(),
                    row.detail.clone(),
                ]
            })
            .collect();
        self.services.get().refresh_model(rows);
    }
}

/// `part` as a fraction of `total` (zero when there is no total).
fn share(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64
    }
}

fn percent(fraction: f64) -> i64 {
    (fraction * 100.0).round() as i64
}
