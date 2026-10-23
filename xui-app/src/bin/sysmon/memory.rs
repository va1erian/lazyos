//! The Memory tab: every process with the memory it holds and what that memory
//! is for, biggest first. A page several processes map is divided between
//! them (`xui_app::sysinfo::procmem`), so the column adds up to what the
//! programs really hold and a program's shared code is counted once.

use xui_app::format::bytes;
use xui_app::sysinfo::procmem::Class;
use xui_app::sysinfo::{ProcessMemory, ProcessTable, Snapshot};
use xui_core::prelude::*;

use crate::Msg;

/// The Memory tab's widgets.
#[derive(Default)]
pub(crate) struct Memory {
    heading: Handle<Label<Msg>>,
    note: Handle<Label<Msg>>,
    pub(crate) processes: Handle<ListView<Msg>>,
}

impl Memory {
    pub(crate) fn layout(&self) -> Layout<Msg> {
        column().padding(8).gap(8).children((
            label("Reading memory…").bind(&self.heading),
            label("").bind(&self.note),
            list()
                .column("Process", Fill)
                .column_right("Memory", 90)
                .column_right("Code", 80)
                .column_right("Heap & data", 90)
                .column_right("Stacks", 70)
                .column_right("Shared", 80)
                .column_right("Files", 70)
                .column_right("Private", 80)
                .column_right("Threads", 60)
                .bind(&self.processes)
                .fill(1),
        ))
    }

    /// Fills the table from one refresh; `None` when the kernel did not say.
    pub(crate) fn show(&self, snapshot: &Snapshot, memory: Option<&ProcessTable>) {
        let Some(table) = memory else {
            self.show_unavailable();
            return;
        };
        let mut rows: Vec<&ProcessMemory> = table.rows.iter().collect();
        rows.sort_by(|a, b| b.footprint().cmp(&a.footprint()).then(a.pid.cmp(&b.pid)));
        let cells: Vec<Vec<String>> = rows.iter().map(|row| cells(snapshot, row)).collect();
        self.processes.get().refresh_model(cells);
        let (held, counted) = (table.footprint(), table.resident());
        self.heading.get().set_text(&format!(
            "Memory — {} processes hold {}",
            table.rows.len(),
            bytes(held)
        ));
        self.note.get().set_text(&format!(
            "Shared pages are divided between the processes using them; counted in full each time they would say {}.",
            bytes(counted),
        ));
    }

    /// Says the figures are not available and clears the table.
    pub(crate) fn show_unavailable(&self) {
        self.heading
            .get()
            .set_text("Memory by process is not available");
        self.note.get().set_text("");
        self.processes
            .get()
            .refresh_model(Vec::<Vec<String>>::new());
    }
}

/// The table row of one process.
fn cells(snapshot: &Snapshot, row: &ProcessMemory) -> Vec<String> {
    let name = snapshot
        .tasks
        .iter()
        .find(|task| task.live() && task.pid == row.pid)
        .map_or_else(
            || format!("pid {}", row.pid),
            |task| task.name().to_string(),
        );
    let class = |class: Class| bytes(row.class_pss[class as usize]);
    vec![
        format!("{name} ({})", row.pid),
        bytes(row.footprint()),
        class(Class::Image),
        class(Class::Private),
        class(Class::Stack),
        class(Class::Shared),
        class(Class::Mapped),
        bytes(row.private()),
        row.threads.to_string(),
    ]
}
