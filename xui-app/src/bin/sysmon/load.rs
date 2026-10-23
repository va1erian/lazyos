//! CPU load between two refreshes: the whole machine's, from the kernel's idle
//! counter, and each program's, from the ticks charged to it since.

use std::collections::HashMap;

use xui_app::sysinfo::{CpuSample, ProcessMemory, ProcessTable, Snapshot, TaskRow, cpu_percent};

/// What the last snapshot said, kept to compare the next one against.
#[derive(Default)]
pub(crate) struct Load {
    previous: Option<CpuSample>,
    /// CPU ticks per task, keyed by pid and name hash so a reused pid starts
    /// from zero.
    ticks: HashMap<(u64, u64), u64>,
}

/// One program's share of the CPU since the last refresh.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProgramLoad {
    pub(crate) name: String,
    pub(crate) pid: u64,
    /// Percent of the interval, to one decimal.
    pub(crate) percent: f64,
    pub(crate) running: bool,
    /// What the program's process costs the machine (its shared pages
    /// divided between the processes mapping them). `None` when the kernel
    /// does not say, and for a thread: its process has the row.
    pub(crate) memory: Option<u64>,
}

impl Load {
    /// Compares `snapshot` with the last one: the machine's load in percent
    /// (`None` on the first call) and every live program's share, busiest
    /// first, with the memory `memory` names for it.
    pub(crate) fn update(
        &mut self,
        snapshot: &Snapshot,
        memory: Option<&ProcessTable>,
    ) -> (Option<u32>, Vec<ProgramLoad>) {
        let sample = snapshot.cpu_sample();
        let elapsed = self
            .previous
            .map_or(0, |previous| sample.ticks.wrapping_sub(previous.ticks));
        let total = self.previous.map(|previous| cpu_percent(previous, sample));
        let mut ticks = HashMap::new();
        let mut programs: Vec<ProgramLoad> = snapshot
            .live_tasks()
            .map(|task| {
                let key = (task.pid, task.name_hash);
                let before = self.ticks.get(&key).copied().unwrap_or(task.cpu_ticks);
                ticks.insert(key, task.cpu_ticks);
                let held = memory.and_then(|table| table.of(task.pid));
                program(
                    task,
                    task.cpu_ticks.saturating_sub(before),
                    elapsed,
                    held.map(ProcessMemory::footprint),
                )
            })
            .collect();
        programs.sort_by(|a, b| b.percent.total_cmp(&a.percent).then(a.pid.cmp(&b.pid)));
        self.previous = Some(sample);
        self.ticks = ticks;
        (total, programs)
    }
}

fn program(task: &TaskRow, used: u64, elapsed: u64, memory: Option<u64>) -> ProgramLoad {
    let percent = if elapsed == 0 {
        0.0
    } else {
        (used as f64 * 1000.0 / elapsed as f64).round().min(1000.0) / 10.0
    };
    let name = match task.name() {
        "" => format!("pid {}", task.pid),
        name => name.to_string(),
    };
    ProgramLoad {
        name,
        pid: task.pid,
        percent,
        running: task.state == xui_app::sysinfo::TaskState::Runnable,
        memory,
    }
}
