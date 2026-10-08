//! The Overview tab, for anyone: how busy the processor is (now and over the
//! last minute), where the memory goes in four named, coloured shares, and
//! which programs use the processor most. The kernel's own counters are on
//! the Advanced tab.

use xui_app::format::bytes;
use xui_app::sysinfo::{MemoryUse, Snapshot};
use xui_core::prelude::*;

use crate::Msg;
use crate::load::ProgramLoad;
use crate::paint::{self, CpuGraph, History, MemoryBar, Share};

/// What each share is called and what it means, in [`Share::ALL`] order.
const SHARES: [(&str, &str); 4] = [
    ("Programs", "Your apps and services"),
    ("System", "LazyOS itself"),
    ("Disk cache", "Recent files, freed on demand"),
    ("Free", "Unused"),
];

/// The Overview tab's widgets.
#[derive(Default)]
pub(crate) struct Overview {
    cpu_value: Handle<Label<Msg>>,
    cpu_note: Handle<Label<Msg>>,
    graph: Handle<CpuGraph>,
    history: History,
    memory_value: Handle<Label<Msg>>,
    memory_note: Handle<Label<Msg>>,
    bar: Handle<MemoryBar>,
    shares: [Handle<Label<Msg>>; 4],
    pub(crate) programs: Handle<ListView<Msg>>,
}

impl Overview {
    pub(crate) fn layout(&self) -> Layout<Msg> {
        column().padding(8).gap(12).children((
            grid([Track::Fill(4), Track::Fill(5)])
                .gap(16)
                .children((self.cpu_card(), self.memory_card())),
            label("Busiest programs: their share of the processor in the last second"),
            list()
                .column("Program", Fill)
                .column_right("Processor", 110)
                .column("Status", 130)
                .bind(&self.programs)
                .fill(1),
        ))
    }

    fn cpu_card(&self) -> Entry<Msg> {
        group(
            "Processor",
            column().gap(6).children((
                label("–").title().bind(&self.cpu_value),
                label("Measuring…").bind(&self.cpu_note),
                build(paint::cpu_graph).bind(&self.graph).fill(1),
                label("Busy time over the last minute").caption(),
            )),
        )
        .into_entry()
    }

    fn memory_card(&self) -> Entry<Msg> {
        let mut legend = Vec::new();
        for ((share, (name, meaning)), value) in
            Share::ALL.into_iter().zip(SHARES).zip(&self.shares)
        {
            legend.push(build(move |ui| paint::swatch(ui, share)).into_entry());
            legend.push(label(name).into_entry());
            legend.push(label("").bind(value).into_entry());
            legend.push(label(meaning).into_entry());
        }
        group(
            "Memory",
            column().gap(6).children((
                label("–").title().bind(&self.memory_value),
                label("").bind(&self.memory_note),
                build(paint::memory_bar).bind(&self.bar),
                grid([Track::Auto, Track::Auto, Track::Auto, Track::Fill(1)])
                    .gap(8)
                    .children(legend),
            )),
        )
        .into_entry()
    }

    /// Shows a refresh: the machine's CPU load (`None` until two snapshots
    /// were compared), the memory shares and the busiest programs.
    pub(crate) fn show(&mut self, snapshot: &Snapshot, cpu: Option<u32>, programs: &[ProgramLoad]) {
        let running = programs.len();
        match cpu {
            Some(percent) => {
                self.history.push(percent);
                self.cpu_value.get().set_text(&format!("{percent}% busy"));
                self.graph.get().set(self.history.samples().to_vec());
            }
            None => self.cpu_value.get().set_text("–"),
        }
        self.cpu_note.get().set_text(&format!(
            "{running} programs running · up {}",
            friendly_uptime(snapshot.ticks)
        ));
        self.show_memory(&snapshot.memory_use());
        let rows: Vec<Vec<String>> = programs
            .iter()
            .map(|program| {
                vec![
                    program.name.clone(),
                    format!("{:.1}%", program.percent),
                    if program.running {
                        "Running"
                    } else {
                        "Waiting"
                    }
                    .to_string(),
                ]
            })
            .collect();
        self.programs.get().refresh_model(rows);
    }

    fn show_memory(&self, memory: &MemoryUse) {
        let total = memory.total();
        self.memory_value.get().set_text(&format!(
            "{} in use ({}%)",
            bytes(memory.used()),
            percent(memory.used(), total)
        ));
        self.memory_note.get().set_text(&format!(
            "of {} · {} available to programs",
            bytes(total),
            bytes(memory.available())
        ));
        let shares = [memory.programs, memory.system, memory.cache, memory.free];
        self.bar.get().set(shares);
        for (value, part) in self.shares.iter().zip(shares) {
            value
                .get()
                .set_text(&format!("{} ({}%)", bytes(part), percent(part, total)));
        }
    }

    /// Says the snapshot cannot be read and clears the last values.
    pub(crate) fn show_unavailable(&self, code: i64) {
        self.cpu_value.get().set_text("–");
        self.cpu_note
            .get()
            .set_text(&format!("System statistics unavailable (error {code})"));
        self.memory_value.get().set_text("–");
        self.memory_note.get().set_text("");
        self.bar.get().set([0; 4]);
        for value in &self.shares {
            value.get().set_text("");
        }
        self.programs.get().refresh_model(Vec::<Vec<String>>::new());
    }
}

/// `part` in whole percent of `total` (zero when there is no total).
pub(crate) fn percent(part: u64, total: u64) -> u64 {
    if total == 0 {
        0
    } else {
        (part as u128 * 100 / total as u128) as u64
    }
}

/// Uptime in words: `45 s`, `12 min`, `3 h 05 min`, `2 days 4 h`.
pub(crate) fn friendly_uptime(ticks: u64) -> String {
    let seconds = ticks / 100;
    let (minutes, hours, days) = (seconds / 60, seconds / 3600, seconds / 86_400);
    match (days, hours, minutes) {
        (0, 0, 0) => format!("{seconds} s"),
        (0, 0, _) => format!("{minutes} min"),
        (0, _, _) => format!("{hours} h {:02} min", minutes % 60),
        (1, _, _) => format!("1 day {} h", hours % 24),
        _ => format!("{days} days {} h", hours % 24),
    }
}
