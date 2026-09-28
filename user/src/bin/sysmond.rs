//! `sysmond` (`SYSD.ELF`): the system monitor service (issue #144).
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
//! The on-disk name is `SYSD.ELF` (8.3-safe: the kernel's FAT reader only
//! resolves short names). `init` starts the service from its manifest.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::panic::PanicInfo;
use user::central;
use user::messenger::{self, errno, registry, services, Error, Message, Parcel};
use user::sys;
use user::sysinfo::{self, Snapshot, TaskState};

/// How often the retained stats topics are republished (PIT ticks, 100 Hz).
///
/// Every publish formats two payloads and re-encodes one event per
/// subscriber; the user heap (`user/src/heap.rs`) recycles those same-sized
/// blocks each tick, so the service's footprint stays flat. The `snapshot`
/// method is the on-demand path for anything that needs a fresh value *now*.
const PUBLISH_TICKS: u64 = 500;
/// How long the service sleeps between message polls.
const IDLE_TICKS: u64 = 5;

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

        match server.recv_with(&mut buffer, Some(sys::clock() + IDLE_TICKS)) {
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
    if let Err(error) = bus.publish(
        "system/stats/memory",
        memory_payload(&snapshot).as_bytes(),
        true,
    ) {
        *central = None;
        return Err(error.errno().unwrap_or(-errno::EINVAL));
    }
    if let Err(error) = bus.publish(
        "system/stats/tasks",
        tasks_payload(&snapshot).as_bytes(),
        true,
    ) {
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
        .filter(|info| info.topic.starts_with("system/stats/"))
        .count() as u64)
}

/// The `system/stats/memory` payload: one `key=value` line of counters.
fn memory_payload(snapshot: &Snapshot) -> String {
    format!(
        "ticks={} frames_total={} frames_live={} frames_free={} slab_live={} slab_peak={} heap_used={} heap_total={}",
        snapshot.ticks,
        snapshot.frames_total,
        snapshot.frames_live,
        snapshot.frames_free,
        snapshot.slab_live,
        snapshot.slab_peak,
        snapshot.heap_used,
        snapshot.heap_total,
    )
}

/// The `system/stats/tasks` payload: a `live=N` line, then one line per live
/// task (`key=value` fields, NUL-trimmed short name).
fn tasks_payload(snapshot: &Snapshot) -> String {
    let mut out = format!("live={}", snapshot.tasks_live);
    for row in snapshot.live_tasks() {
        let wait = if row.state == TaskState::Blocked {
            row.wait.label()
        } else {
            ""
        };
        out.push_str(&format!(
            "\npid={} ppid={} state={} wait={} class={} cpu={} name={}",
            row.pid,
            row.ppid,
            row.state.label(),
            wait,
            row.class.label(),
            row.cpu_ticks,
            row.name(),
        ));
    }
    out
}

/// Dispatch one inbound message: a `snapshot` call on the system interface.
fn dispatch(message: &Message) -> messenger::Result<Parcel> {
    match message.interface_id() {
        services::SYSMOND_INTERFACE => match message.method() {
            services::sysmond_method::SNAPSHOT => {
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
