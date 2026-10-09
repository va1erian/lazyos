//! The `sysmon` window's widgets: the header with the Help button, the three
//! tabs (Overview, Services, Advanced), the compact meters and the status bar.
//!
//! Everything is a standard xui widget placed by a layout, apart from the
//! Overview's graph, bar and swatches (`paint.rs`). Resizing, HiDPI and the
//! theme come from the layout and the widgets.

use xui_app::format::bytes;
use xui_app::services::Services;
use xui_app::sys::errno;
use xui_app::sysinfo::Snapshot;
use xui_core::Lucide;
use xui_core::app::Ui;
use xui_core::backend::WidgetId;
use xui_core::prelude::*;

use crate::advanced::{Advanced, GAUGE, share};
use crate::load::ProgramLoad;
use crate::overview::{Overview, friendly_uptime};
use crate::{Msg, View};

/// The keys the header reminds of.
const KEYS: &str = "[o] overview  [s] services  [a] advanced  [c] compact  [F1] help";

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
    help: Handle<Button<Msg>>,
    pub(crate) tabs: Handle<Tabs<Msg>>,
    pub(crate) overview: Overview,
    pub(crate) advanced: Advanced,
    services_heading: Handle<Label<Msg>>,
    services_note: Handle<Label<Msg>>,
    services: Handle<ListView<Msg>>,
    compact: [Meter; 3],
    pub(crate) status: Handle<StatusBar<Msg>>,
}

impl Widgets {
    pub(crate) fn new() -> Widgets {
        Widgets::default()
    }

    /// The whole window: a header, the tabs (or, when compact, the meters)
    /// and the status bar.
    pub(crate) fn layout(&self) -> Layout<Msg> {
        let mut meters = Vec::new();
        for (meter, name) in self
            .compact
            .iter()
            .zip(["Processor", "Memory", "Disk cache"])
        {
            meters.extend(meter.cells(name));
        }
        column().padding(12).gap(10).children((
            row().gap(16).align(Align::Center).children((
                label("System Monitor").title(),
                label(KEYS).bind(&self.hint).fill(1),
                button("Help")
                    .icon(Lucide::CircleHelp)
                    .tooltip("Open the System Monitor guide in Docs (F1)")
                    .on_click(Msg::Help)
                    .bind(&self.help),
            )),
            tabs()
                .page("Overview", self.overview.layout())
                .page("Services", self.services_page())
                .page("Advanced", self.advanced.layout())
                .on_change(|index| Msg::Show(View::from_index(index)))
                .bind(&self.tabs)
                .fill(1),
            grid([Track::Auto, Track::Fill(1), Track::Auto])
                .gap(8)
                .children(meters),
            status_bar(&["", "", "", ""]).bind(&self.status),
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
            self.help.get().id(),
            self.tabs.get().id(),
            self.status.get().id(),
        ] {
            ui.set_visible(id, !compact);
        }
    }

    /// Fills every tab and the compact meters from one refresh.
    pub(crate) fn show_snapshot(
        &mut self,
        snapshot: &Snapshot,
        cpu: Option<u32>,
        programs: &[ProgramLoad],
    ) {
        self.overview.show(snapshot, cpu, programs);
        self.advanced.show(snapshot);
        let memory = snapshot.memory_use();
        let [cpu_meter, memory_meter, cache_meter] = &self.compact;
        let cpu = cpu.unwrap_or(0);
        cpu_meter.show(f64::from(cpu) / 100.0, format!("{cpu}%"));
        memory_meter.show(
            share(memory.used(), memory.total()),
            format!("{} / {}", bytes(memory.used()), bytes(memory.total())),
        );
        cache_meter.show(share(memory.cache, memory.total()), bytes(memory.cache));
    }

    /// The status bar: uptime, the refresh rate, refreshes and the snapshot
    /// source (or the last refresh's error).
    pub(crate) fn show_status(&self, ticks: u64, refreshes: u64, error: Option<i64>) {
        let source = match error {
            Some(code) => format!("Last update failed (errno {code})"),
            None => "Kernel statistics (syscall 14)".to_string(),
        };
        let status = self.status.get();
        status.set_text(0, &format!("Up {}", friendly_uptime(ticks)));
        status.set_text(1, "Updates every second");
        status.set_text(2, &format!("{refreshes} updates"));
        status.set_text(3, &source);
    }

    /// Says the snapshot cannot be read and clears the values it would show,
    /// so no view keeps the last successful read; the compact meters (shown
    /// without the tabs or the status bar) carry the errno.
    pub(crate) fn show_unavailable(&self, code: i64) {
        self.overview.show_unavailable(code);
        self.advanced.show_unavailable(code);
        for meter in &self.compact {
            meter.show(0.0, format!("errno {code}"));
        }
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
        let notes = service_notes(view);
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

/// What the Services tab says above an incomplete table: a refusal is not an
/// empty table, so each source's errno is named for what it means.
pub(crate) fn service_notes(view: &Services) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(code) = view.init_error {
        notes.push(source_note("init", code));
    }
    if let Some(code) = view.health_error {
        notes.push(source_note("healthd", code));
    }
    if view.rows.is_empty() && view.init_error.is_none() {
        notes.push("No supervised services (is the image built with LAZYOS_SERVICES=1?)".into());
    }
    notes
}

fn source_note(service: &str, code: i64) -> String {
    match -code {
        errno::EACCES | errno::EPERM => {
            format!("{service} refused the request (errno {code}: not permitted)")
        }
        errno::ENOENT => {
            format!("{service} not found or not visible to this app (errno {code})")
        }
        _ => format!("{service} unavailable (errno {code})"),
    }
}
