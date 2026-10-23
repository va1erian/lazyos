//! webd's process monitor: where the memory of webd and of the engine processes it started goes.
//!
//! Counting a process's memory by its resident pages overstates a program like webd, which is the
//! sum of several processes running the same big static image: the image's code and read-only data
//! are one set of frames in the page cache that every process maps, and the clients' pixel
//! buffers are mapped into webd as well as into the client. The kernel divides each page between
//! the processes that map it (`lazyos_sys::sysinfo::procmem`: proportional size), so the figures
//! here add up to what the engine really holds, and [`Totals::sharing`] says how much a plain sum
//! of resident sizes would have counted twice.
//!
//! This module is the pure part: it picks webd's processes out of the system's, gives each its
//! role and writes the report as `WEBD:MEM:*` lines. [`crate::memory`] reads the kernel and
//! prints them on request, so nothing here polls.
//!
//! ```text
//! WEBD:MEM:PROC role=web pid=14 threads=12 name=webproce memory=.. private=.. resident=.. code=..
//!     heap=.. stack=.. shared=.. files=.. tables=..
//! WEBD:MEM:CLIENT label=app:os.lazy.docs views=2 slots=..
//! WEBD:MEM:TOTAL processes=3 memory=.. private=.. resident=.. sharing=..
//! ```
//!
//! Every size is in bytes. `memory` is the process's proportional share plus its page tables,
//! `private` what only it holds and `resident` every page it maps, shared ones in full; `code`,
//! `heap`, `stack`, `shared` and `files` split the proportional share by what the pages are for.

/// A process of the system, from the scheduler's table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proc {
    pub pid: u64,
    pub ppid: u64,
    /// The task's short name (the kernel keeps eight bytes of it).
    pub name: String,
}

/// What the kernel says one process holds, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub pid: u64,
    pub threads: u64,
    /// Proportional share of the pages it maps, plus its page tables.
    pub memory: u64,
    /// Bytes only this process holds, page tables included.
    pub private: u64,
    /// Every page it maps, shared pages in full.
    pub resident: u64,
    pub tables: u64,
    /// The proportional share by what the pages are for.
    pub code: u64,
    pub heap: u64,
    pub stack: u64,
    pub shared: u64,
    pub files: u64,
}

/// What a process of webd's is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// webd itself: the service, WebKit's run loop and the clients' buffers.
    Service,
    /// A web process (`webprocess`): one per client app and profile, running the pages.
    Web,
    /// The network process (`networkprocess`): one for every profile.
    Network,
    /// A child of webd that is neither (a helper a later version starts).
    Other,
}

impl Role {
    fn of(name: &str) -> Role {
        if name.starts_with("webproc") {
            Role::Web
        } else if name.starts_with("networkp") {
            Role::Network
        } else {
            Role::Other
        }
    }

    fn label(self) -> &'static str {
        match self {
            Role::Service => "service",
            Role::Web => "web",
            Role::Network => "network",
            Role::Other => "other",
        }
    }
}

/// One process of webd's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub role: Role,
    pub name: String,
    pub usage: Usage,
}

/// What a client holds in webd: its views and the bytes of its buffers attached to them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientLine {
    pub label: String,
    pub views: usize,
    pub slot_bytes: u64,
}

/// The sums of a report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub processes: usize,
    pub memory: u64,
    pub private: u64,
    pub resident: u64,
    /// What a sum of resident sizes counts more than the processes hold: pages that several
    /// of them map, counted once for each.
    pub sharing: u64,
}

/// webd's processes, the service first and then the others by what they hold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub rows: Vec<Row>,
}

/// The processes of `usages` that are `service` or descend from it, through `procs`' parent links.
pub fn build(service: u64, procs: &[Proc], usages: &[Usage]) -> Report {
    let mut members = vec![service];
    // A process joins when its parent is a member; one pass per generation, and a table of
    // `n` processes has at most `n` generations.
    for _ in 0..procs.len() {
        let before = members.len();
        for proc in procs {
            if proc.pid != proc.ppid && members.contains(&proc.ppid) && !members.contains(&proc.pid)
            {
                members.push(proc.pid);
            }
        }
        if members.len() == before {
            break;
        }
    }
    let mut rows: Vec<Row> = usages
        .iter()
        .filter(|usage| members.contains(&usage.pid))
        .map(|usage| {
            let name = procs
                .iter()
                .find(|proc| proc.pid == usage.pid)
                .map_or_else(|| format!("pid{}", usage.pid), |proc| proc.name.clone());
            let role = if usage.pid == service {
                Role::Service
            } else {
                Role::of(&name)
            };
            Row {
                role,
                name,
                usage: *usage,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        (a.role != Role::Service)
            .cmp(&(b.role != Role::Service))
            .then(b.usage.memory.cmp(&a.usage.memory))
            .then(a.usage.pid.cmp(&b.usage.pid))
    });
    Report { rows }
}

impl Report {
    pub fn totals(&self) -> Totals {
        let sum = |pick: fn(&Usage) -> u64| self.rows.iter().map(|row| pick(&row.usage)).sum();
        let (memory, resident): (u64, u64) = (sum(|u| u.memory), sum(|u| u.resident));
        Totals {
            processes: self.rows.len(),
            memory,
            private: sum(|u| u.private),
            resident,
            sharing: resident.saturating_sub(memory),
        }
    }

    /// The report as marker text (without the `WEBD:` the trace adds): a line per process, a line
    /// per client and the total last.
    pub fn lines(&self, clients: &[ClientLine]) -> Vec<String> {
        let mut out = Vec::with_capacity(self.rows.len() + clients.len() + 1);
        for row in &self.rows {
            let u = &row.usage;
            out.push(format!(
                "MEM:PROC role={} pid={} threads={} memory={} private={} resident={} code={} heap={} stack={} shared={} files={} tables={} name={}",
                row.role.label(),
                u.pid,
                u.threads,
                u.memory,
                u.private,
                u.resident,
                u.code,
                u.heap,
                u.stack,
                u.shared,
                u.files,
                u.tables,
                row.name
            ));
        }
        for client in clients {
            out.push(format!(
                "MEM:CLIENT views={} slots={} label={}",
                client.views, client.slot_bytes, client.label
            ));
        }
        let total = self.totals();
        out.push(format!(
            "MEM:TOTAL processes={} memory={} private={} resident={} sharing={}",
            total.processes, total.memory, total.private, total.resident, total.sharing
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u64, ppid: u64, name: &str) -> Proc {
        Proc {
            pid,
            ppid,
            name: name.into(),
        }
    }

    fn usage(pid: u64, memory: u64, resident: u64) -> Usage {
        Usage {
            pid,
            threads: 1,
            memory,
            resident,
            private: memory / 2,
            ..Usage::default()
        }
    }

    /// webd (pid 5) with two web processes and a network process, beside an unrelated program
    /// and a web process of somebody else's.
    fn table() -> (Vec<Proc>, Vec<Usage>) {
        (
            vec![
                proc(1, 0, "init"),
                proc(5, 1, "webd-app"),
                proc(9, 5, "webproce"),
                proc(11, 5, "webproce"),
                proc(12, 5, "networkp"),
                proc(20, 1, "xuid"),
                proc(30, 20, "webproce"),
                // A thread of a web process reports the process's parent.
                proc(13, 5, "webproce"),
            ],
            vec![
                usage(1, 100, 100),
                usage(5, 400, 900),
                usage(9, 300, 700),
                usage(11, 250, 700),
                usage(12, 90, 200),
                usage(20, 500, 500),
                usage(30, 999, 999),
            ],
        )
    }

    #[test]
    fn it_picks_webd_and_what_it_started() {
        let (procs, usages) = table();
        let report = build(5, &procs, &usages);
        let pids: Vec<u64> = report.rows.iter().map(|row| row.usage.pid).collect();
        assert_eq!(
            pids,
            [5, 9, 11, 12],
            "the service first, the rest by memory"
        );
        let roles: Vec<Role> = report.rows.iter().map(|row| row.role).collect();
        assert_eq!(roles, [Role::Service, Role::Web, Role::Web, Role::Network]);
    }

    #[test]
    fn grandchildren_belong_to_webd_too() {
        let (mut procs, mut usages) = table();
        procs.push(proc(40, 12, "curl"));
        usages.push(usage(40, 10, 10));
        let report = build(5, &procs, &usages);
        assert!(report
            .rows
            .iter()
            .any(|row| row.usage.pid == 40 && row.role == Role::Other));
    }

    #[test]
    fn a_parent_loop_does_not_hang() {
        let procs = vec![proc(5, 6, "webd-app"), proc(6, 5, "x"), proc(7, 7, "y")];
        let report = build(5, &procs, &[usage(5, 1, 1), usage(6, 1, 1), usage(7, 1, 1)]);
        assert_eq!(report.rows.len(), 2);
    }

    #[test]
    fn the_totals_say_how_much_sharing_a_plain_sum_would_count_twice() {
        let (procs, usages) = table();
        let total = build(5, &procs, &usages).totals();
        assert_eq!(total.processes, 4);
        // 400 + 300 + 250 + 90 held; 900 + 700 + 700 + 200 mapped.
        assert_eq!((total.memory, total.resident), (1040, 2500));
        assert_eq!(total.sharing, 1460);
        assert_eq!(total.private, 200 + 150 + 125 + 45);
    }

    #[test]
    fn the_lines_are_one_per_process_and_the_total_is_last() {
        let (procs, usages) = table();
        let clients = [ClientLine {
            label: "app:os.lazy.docs".into(),
            views: 2,
            slot_bytes: 4096,
        }];
        let lines = build(5, &procs, &usages).lines(&clients);
        assert_eq!(lines.len(), 4 + 1 + 1);
        assert!(lines[0].starts_with("MEM:PROC role=service pid=5 threads=1 memory=400 "));
        assert!(
            lines[0].ends_with(" name=webd-app"),
            "free text last: {}",
            lines[0]
        );
        assert_eq!(
            lines[4],
            "MEM:CLIENT views=2 slots=4096 label=app:os.lazy.docs"
        );
        assert_eq!(
            lines[5],
            "MEM:TOTAL processes=4 memory=1040 private=520 resident=2500 sharing=1460"
        );
    }

    #[test]
    fn nothing_running_is_an_empty_report() {
        let report = build(5, &[], &[]);
        assert!(report.rows.is_empty());
        assert_eq!(report.totals(), Totals::default());
        assert_eq!(
            report.lines(&[]),
            ["MEM:TOTAL processes=0 memory=0 private=0 resident=0 sharing=0"]
        );
    }
}
