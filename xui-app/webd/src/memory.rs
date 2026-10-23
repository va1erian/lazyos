//! The process monitor on the real system: read the scheduler's table and the kernel's per-process
//! memory ([`xui_app::sysinfo::procmem`]) and print webd's report on request.
//!
//! Asked with `Launch os.lazy.webd` and the path `/memory` (`init` sends the running instance
//! `Reopen("/memory")`, as for the self-test): one report, as `WEBD:MEM:*` lines
//! ([`crate::procmon`] has the format). Nothing runs between requests, so the monitor costs nothing
//! while it is not looked at; the walk behind one visits every resident page of every process, which
//! takes milliseconds (more with gigabytes of shared pages), which is why it is asked for and not
//! repeated.

use xui_app::sysinfo::procmem::{Class, ProcessMemory};
use xui_app::sysinfo::{self, procmem, PAGE};
use xui_app::trace::{self, Budget, Channel};

use crate::procmon::{self, ClientLine, Proc, Usage};

/// A report is a few lines and a walk over every process's pages: at most 3 per ten seconds, so a
/// caller that asks in a loop cannot keep the service busy.
static REPORTS: Budget = Budget::new(3, 1000);

/// The `Reopen` argument that asks for a report (`init` takes one absolute path as `args`).
pub const ARG: &str = "/memory";

fn usage(row: &ProcessMemory) -> Usage {
    let class = |class: Class| row.class_pss[class as usize];
    Usage {
        pid: row.pid,
        threads: row.threads,
        memory: row.footprint(),
        private: row.private(),
        resident: row.resident(),
        tables: row.table_pages * PAGE,
        code: class(Class::Image),
        heap: class(Class::Private),
        stack: class(Class::Stack),
        shared: class(Class::Shared),
        files: class(Class::Mapped),
    }
}

/// Print webd's report: its processes as the kernel counts them, and `clients`, what each client
/// holds in the service.
pub fn report(clients: &[ClientLine]) {
    if !trace::within(&REPORTS, Channel::Webd) {
        return;
    }
    let snapshot = match sysinfo::snapshot() {
        Ok(snapshot) => snapshot,
        Err(code) => {
            trace::fail!(Channel::Webd, "MEM", "snapshot err={}", -code);
            return;
        }
    };
    let table = match procmem::read() {
        Ok(table) => table,
        Err(code) => {
            trace::fail!(Channel::Webd, "MEM", "process memory err={}", -code);
            return;
        }
    };
    let procs: Vec<Proc> = snapshot
        .live_tasks()
        .map(|task| Proc {
            pid: task.pid,
            ppid: task.ppid,
            name: task.name().to_string(),
        })
        .collect();
    let usages: Vec<Usage> = table.rows.iter().map(usage).collect();
    // The process id is the thread group's leader, which `getpid` answers in every thread.
    let report = procmon::build(u64::from(std::process::id()), &procs, &usages);
    for line in report.lines(clients) {
        trace::line!(Channel::Webd, "{line}");
    }
}
