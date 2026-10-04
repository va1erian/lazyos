//! `sysmond` (`/system/bin/sysmond`): the system monitor service (issue #144).
//!
//! `sysmond` wraps the kernel's read-only system-stats syscall (14) in the
//! Messenger fabric:
//!
//! * it registers [`services::SYSMOND_NAME`] and serves
//!   `os.lazy.system.v1`'s `snapshot` method, returning one fixed-layout
//!   [`user::sysinfo::Snapshot`] as raw bytes so a client decodes the same
//!   block `top` reads directly from the kernel;
//! * it republishes the snapshot as the retained topics
//!   `system/stats/memory` and `system/stats/tasks` through `messengerd`'s
//!   central broker ([`user::central`]), so a dashboard subscribes once and
//!   is handed the latest values, then every update — and the hardware
//!   fabric view sees the stats topics alongside every other service's;
//! * it prints one machine-parseable line when it registers and one when the
//!   first snapshot lands (`SYSMOND:REGISTER:PASS`, `SYSMOND:SNAPSHOT:PASS`),
//!   which is the headless boot evidence.
//!
//! Permission: the snapshot is readable by every task (see
//! `kernel/src/sysinfo.rs`); it carries counters, pids, states, classes, CPU
//! ticks and names, and deliberately no addresses or credentials, so the
//! monitor is safe to expose to unprivileged clients.
//!
//! `init` starts the service from its manifest. With `demo=1` in the
//! manifest arguments it spawns `top` (`/system/bin/top`), its one-shot
//! evidence client, and reaps it: `top` exits once it has printed its verdict,
//! so it is not a supervised service.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::panic::PanicInfo;
use messenger_generated::os_lazy_sysmond_v1 as stats;
use user::central;
use user::messenger::{self, errno, registry, services, wait, Error, Message, Parcel};
use user::sys;
use user::sysinfo::{self, Snapshot, TaskState};

/// How often the retained stats topics are republished (PIT ticks, 100 Hz).
///
/// Every publish encodes two typed payloads and re-encodes one event per
/// subscriber; the user heap (`user/src/heap.rs`) recycles those same-sized
/// blocks each tick, so the service's footprint stays flat. The `snapshot`
/// method is the on-demand path for anything that needs a fresh value *now*.
const PUBLISH_TICKS: u64 = 500;
/// The evidence program `demo=1` spawns once the first snapshot is retained.
const DEMO_PROGRAM: &str = fhs::bin::TOP;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("sysmond: system monitor service (issue #144)\n");
    if let Err(error) = run() {
        sys::write_str("sysmond: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Register, publish the retained topics on a timer, and serve `snapshot`.
fn run() -> messenger::Result<()> {
    let (published, server) = messenger::create_pair()?;
    registry::register(
        services::SYSMOND_NAME,
        &published,
        &[services::SYSMOND_INTERFACE],
        0,
    )?;
    sys::write_str("sysmond: registered as ");
    sys::write_str(services::SYSMOND_NAME);
    sys::write_str("\n");
    sys::write_str("SYSMOND:REGISTER:PASS\n");

    // The central broker connection appears when `messengerd` has finished
    // registering its name (it is the supervisor's first service, but the two
    // race at boot, so the first publishes may retry).
    let mut central: Option<central::Bus> = None;
    // One receive buffer for the life of the service: the user bump allocator
    // never reclaims per-call buffers, so the loop must not allocate one per
    // message (the encoded reply still allocates; that is the Messenger API's
    // current shape and is bounded by the request rate).
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let mut next_publish = 0u64;
    let mut announced = false;
    let mut demo_pending = demo_from_args();
    let mut demo_running = false;

    loop {
        let now = sys::clock();
        if now >= next_publish {
            match publish_stats(&mut central, announced) {
                Ok(topics) if !announced => {
                    announced = true;
                    // The broker's own view of the `system/stats/*` topics is
                    // the evidence that both reached the central broker.
                    sys::write_str(&format!("SYSMOND:SNAPSHOT:PASS topics={topics}\n"));
                }
                Ok(_) => {}
                Err(code) => {
                    sys::write_str(&format!("SYSMOND:SNAPSHOT:FAIL {code}\n"));
                }
            }
            next_publish = sys::clock() + PUBLISH_TICKS;
        }
        // Start `top` only once the retained topics exist, so its first
        // snapshot already has something to show.
        if demo_pending && announced {
            demo_pending = false;
            demo_running = spawn_demo();
        }

        // Park until a request, the demo child's exit, or the next publish:
        // an idle `sysmond` wakes once per publish (P7).
        let doorbells = if demo_running { wait::WAIT_CHILD } else { 0 };
        let ready = match wait::wait_any(&[server], doorbells, Some(next_publish)) {
            Ok(ready) => ready,
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => 0,
            Err(error) => return Err(error),
        };
        if ready & wait::CHILD_READY != 0 && sys::wait(sys::clock().max(1)).is_some() {
            demo_running = false;
        }
        if ready & 1 == 0 {
            continue;
        }
        match server.recv_with(&mut buffer, Some(messenger::EXPIRED_DEADLINE)) {
            Ok(message) => {
                let reply = match dispatch(&message) {
                    Ok(parcel) => parcel,
                    // A failed request still gets an answer, or its caller
                    // would wait forever: a structured error for the request's
                    // own interface and method, carrying the original errno,
                    // so a failure is never mistaken for a success.
                    Err(error) => {
                        services::error_reply(message.interface_id(), message.method(), error)
                    }
                };
                if let Some(txn) = message.txn {
                    // A caller whose deadline passed is a normal scheduling
                    // race, not a service failure; keep serving.
                    if let Err(error) = server.reply(txn, &reply) {
                        if error.errno() != Some(-errno::ENOENT) {
                            return Err(error);
                        }
                    }
                }
            }
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => {}
            Err(error) => return Err(error),
        }
    }
}

/// Whether the manifest asked for the `top` demo (`demo=1`).
fn demo_from_args() -> bool {
    sys::args().skip(1).any(|arg| arg == "demo=1")
}

/// Spawn `top` as a child of this service; returns whether it started.
fn spawn_demo() -> bool {
    match sys::spawn_native(DEMO_PROGRAM, &[]) {
        Some(pid) => {
            let name = fhs::bin::name(DEMO_PROGRAM);
            sys::write_str(&format!("sysmond: started demo {name} (pid {pid})\n"));
            true
        }
        None => {
            sys::write_str(&format!("sysmond: demo {DEMO_PROGRAM} spawn failed\n"));
            false
        }
    }
}

/// One `snapshot` call plus the two retained topic publishes; returns how many
/// `system/stats/*` topics the central broker reports afterwards, or `0` once
/// `announced` is true and the caller no longer looks at the count (skipping
/// `list()` then saves a broker round trip and its decoded-reply allocation
/// on every publish cycle for the rest of the service's life).
fn publish_stats(central: &mut Option<central::Bus>, announced: bool) -> Result<u64, i64> {
    if central.is_none() {
        *central = central::Bus::connect_retry(4).ok();
    }
    let Some(bus) = central.as_mut() else {
        return Err(-errno::ENOENT);
    };
    let snapshot = sysinfo::snapshot()?;
    // On any failure below, the endpoint itself may be the cause (e.g. the
    // broker restarted), so the cached bus is dropped rather than kept: the
    // next call's `central.is_none()` check above then reconnects instead of
    // retrying a dead handle forever.
    if let Err(error) = stats::publish_system_stats_memory(bus, &memory_stats(&snapshot)) {
        *central = None;
        return Err(error.errno().unwrap_or(-errno::EINVAL));
    }
    if let Err(error) = stats::publish_system_stats_tasks(bus, &tasks_stats(&snapshot)) {
        *central = None;
        return Err(error.errno().unwrap_or(-errno::EINVAL));
    }
    if announced {
        return Ok(0);
    }
    let list = match bus.list() {
        Ok(list) => list,
        Err(error) => {
            *central = None;
            return Err(error.errno().unwrap_or(-errno::EINVAL));
        }
    };
    Ok(list
        .iter()
        .filter(|info| is_stats_topic(&info.topic))
        .count() as u64)
}

/// Whether `topic` is one of the declared `system/stats/*` topics, so the
/// evidence count is derived from the interface constants rather than a
/// hand-typed prefix.
fn is_stats_topic(topic: &str) -> bool {
    topic == stats::TOPIC_SYSTEM_STATS_MEMORY || topic == stats::TOPIC_SYSTEM_STATS_TASKS
}

/// The `system/stats/memory` payload: the snapshot's memory counters.
fn memory_stats(snapshot: &Snapshot) -> stats::MemoryStats {
    stats::MemoryStats {
        ticks: snapshot.ticks,
        frames_total: snapshot.frames_total,
        frames_live: snapshot.frames_live,
        frames_free: snapshot.frames_free,
        slab_live: snapshot.slab_live,
        slab_peak: snapshot.slab_peak,
        heap_used: snapshot.heap_used,
        heap_total: snapshot.heap_total,
    }
}

/// The `system/stats/tasks` payload: the live count and one row per live task,
/// with the short labels the text view used to print and the NUL-trimmed name.
fn tasks_stats(snapshot: &Snapshot) -> stats::TasksStats {
    let tasks = snapshot
        .live_tasks()
        .map(|row| stats::TaskRow {
            pid: row.pid,
            ppid: row.ppid,
            state: String::from(row.state.label()),
            wait: if row.state == TaskState::Blocked {
                String::from(row.wait.label())
            } else {
                String::new()
            },
            class: String::from(row.class.label()),
            cpu: row.cpu_ticks,
            name: String::from(row.name()),
        })
        .collect();
    stats::TasksStats {
        live: snapshot.tasks_live,
        tasks,
    }
}

/// Dispatch one inbound message: a `snapshot` call on the system interface.
fn dispatch(message: &Message) -> messenger::Result<Parcel> {
    match message.interface_id() {
        services::SYSMOND_INTERFACE => match message.method() {
            services::sysmond::METHOD_SNAPSHOT => {
                let snapshot = sysinfo::snapshot().map_err(Error::Errno)?;
                services::sysinfo_reply(&snapshot)
            }
            _ => Err(Error::Errno(-errno::EINVAL)),
        },
        _ => Err(Error::Errno(-errno::EINVAL)),
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
