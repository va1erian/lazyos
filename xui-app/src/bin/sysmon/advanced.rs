//! The Advanced tab: the kernel's own counters (physical frames, the slab
//! allocator, the kernel heap) and the full task table with scheduler states,
//! classes and CPU ticks, for whoever debugs the kernel.

use xui_app::format::bytes;
use xui_app::sysinfo::{MAX_TASKS, Snapshot};
use xui_core::prelude::*;

use crate::Msg;

/// A gauge's progress bar counts in thousandths.
pub(crate) const GAUGE: i32 = 1000;

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
        for (index, value) in self.values.iter().enumerate() {
            value
                .get()
                .set_text(lines.get(index).map_or("", String::as_str));
        }
    }
}

/// The Advanced tab's widgets.
pub(crate) struct Advanced {
    frames: Gauge,
    slab: Gauge,
    heap: Gauge,
    tasks_heading: Handle<Label<Msg>>,
    pub(crate) tasks: Handle<ListView<Msg>>,
}

impl Default for Advanced {
    fn default() -> Advanced {
        Advanced {
            frames: Gauge::new(6),
            slab: Gauge::new(4),
            heap: Gauge::new(3),
            tasks_heading: Handle::new(),
            tasks: Handle::new(),
        }
    }
}

impl Advanced {
    pub(crate) fn layout(&self) -> Layout<Msg> {
        column().padding(8).gap(10).children((
            grid([Track::Fill(1); 3]).gap(16).children((
                self.frames.card(
                    "Frames",
                    &[
                        "live",
                        "free",
                        "reserved",
                        "block cache",
                        "slabs",
                        "double frees",
                    ],
                ),
                self.slab
                    .card("Slab", &["live", "peak", "oversized", "frames"]),
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

    /// Fills the gauges and the task table from `snapshot`.
    pub(crate) fn show(&self, s: &Snapshot) {
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
                s.cache_frames.to_string(),
                s.slab_frames.to_string(),
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
                s.slab_frames.to_string(),
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

    /// Clears every value so no tab keeps the last successful read.
    pub(crate) fn show_unavailable(&self, code: i64) {
        self.tasks_heading.get().set_text(&format!(
            "System snapshot unavailable: syscall 14 returned errno {code}"
        ));
        for gauge in [&self.frames, &self.slab, &self.heap] {
            gauge.show(0.0, &[]);
        }
        self.tasks.get().refresh_model(Vec::<Vec<String>>::new());
    }
}

/// `part` as a fraction of `total` (zero when there is no total).
pub(crate) fn share(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64
    }
}

fn percent(fraction: f64) -> i64 {
    (fraction * 100.0).round() as i64
}
