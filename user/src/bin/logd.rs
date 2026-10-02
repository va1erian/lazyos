//! `logd` (`LOGD.ELF`): the structured, hash-chained event log service
//! (issues #93 and #508).
//!
//! `logd` is the S2 event log from the platform plan section 4.9. It:
//!
//! * subscribes to the system topics: `system/events/#` on `init` (service
//!   starts/stops/crashes and login events), `system/health/+` on `healthd`
//!   (the declared retained rows, aggregate included), and `system/events/#`
//!   on `messengerd`'s central broker (the events services publish centrally:
//!   `mimed`'s launch records, the clipboard audit trail, `pkgd`'s events);
//! * samples the fabric audit counters and appends a
//!   `system/events/security/denial` record whenever they advance, which is
//!   the interim signal until the kernel exposes audit records to userspace;
//! * chains every record with FNV-1a over `(previous hash, seq, tick, topic,
//!   detail)` in a bounded in-memory ring, so `Verify` detects tampering with
//!   any retained record;
//! * appends every record to a persistent journal, `/logs/<source>.log`
//!   ([`journal`], `libs/logstore`): one file per source (the service
//!   segment of `system/events/<source>/...` or `system/health/<source>`;
//!   the denial samples go to `kernel.log`, anything else and any source
//!   name outside `[a-z0-9_-]{1,32}` to `system.log`). Lines are
//!   `seq tick topic detail hash`, tab-separated and escaped, each boot
//!   starts the files it touches with a `boot` line, and the hashes chain per
//!   boot. A file is rotated at 256 KiB (`.1`, `.2`) and `logd`'s files stay
//!   within 8 MiB together; `pkg.log` is `pkgd`'s and left alone. Appends are
//!   buffered and flushed every 32 records or 100 ticks;
//! * serves `os.lazy.logd.v1` ([`serve`]): `Tail`/`Count`/`Verify` over the
//!   ring for `messengerctl log`, and `Sources`/`TailFile` over the journals
//!   for uid 0 only.
//!
//! Without a writable `/logs` the ring is the only store: `logd` prints
//! `LOGD:STORE:ABSENT` and reports `degraded` to `healthd`; a write that fails
//! later (a full disk) degrades the same way without losing the ring.
//!
//! On `init`'s lifecycle `Shutdown` (docs/shutdown.md) it drains its feeds a
//! last time, flushes and fsyncs the journals, prints `LOGD:STOP` with the
//! record count, the chain's verdict and the records persisted this boot, and
//! exits 0.
//!
//! Declared payloads are decoded back to their historic `key=value` text by
//! [`payload`], with the raw-bytes fallback kept for undeclared topics.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "logd/feeds.rs"]
mod feeds;
#[path = "logd/journal.rs"]
mod journal;
#[path = "logd/log.rs"]
mod log;
#[path = "logd/payload.rs"]
mod payload;
#[path = "logd/ring.rs"]
mod ring;
#[path = "logd/serve.rs"]
mod serve;

use alloc::format;
use core::panic::PanicInfo;
use user::messenger::services::lifecycle;
use user::messenger::{self, registry, services, Error};
use user::sys;

use feeds::Feeds;
use journal::Journals;
use log::Log;

/// How long the service serves queries before checking its feeds again.
const POLL_TICKS: u64 = 2;
/// Deadline for the best-effort health report, so a busy `healthd` cannot
/// stall the log.
const HEALTH_TICKS: u64 = 10;
/// How often an undelivered health report is retried.
const HEALTH_RETRY_TICKS: u64 = 50;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("logd: structured event log (issue #93)\n");
    if let Err(error) = run() {
        sys::write_str("logd: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Register the log and serve queries while feeding it from the system topics.
fn run() -> messenger::Result<()> {
    let (published, server) = messenger::create_pair()?;
    registry::register(
        services::LOGD_NAME,
        &published,
        &[services::LOGD_INTERFACE, lifecycle::INTERFACE],
        0,
    )?;
    let mut log = Log::new(Journals::open(sys::clock()));
    let mut feeds = Feeds::new();
    // The health status last delivered to `healthd` (`None` until it is up).
    let mut reported: Option<&'static str> = None;
    let mut next_report = 0u64;
    // Reused receive buffer: large per-call buffers are never reclaimed by
    // the user heap, so the polling loop must not allocate one per message.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];

    loop {
        feeds.connect();
        feeds.drain_local(&mut log, &mut buffer);
        feeds.drain_central(&mut log, &mut buffer);
        let now = sys::clock();
        feeds.poll(&mut log, now);
        log.journals.tick(now);
        if now >= next_report {
            report_health(&log.journals, &mut reported);
            next_report = now + HEALTH_RETRY_TICKS;
        }

        match server.recv_with(&mut buffer, Some(sys::clock() + POLL_TICKS)) {
            Ok(message) => {
                // An orderly shutdown (docs/shutdown.md): take in what the
                // feeds still hold (the services' last stop events), make the
                // journals durable, then go.
                if let Some(reason) = lifecycle::stop_requested(&message) {
                    feeds.drain_local(&mut log, &mut buffer);
                    log.journals.sync(sys::clock());
                    sys::write_str(&format!(
                        "LOGD:STOP records={} verified={} persisted={} reason=\"{reason}\"\n",
                        log.ring.total,
                        log.ring.verify().0,
                        log.journals.persisted()
                    ));
                    return Ok(());
                }
                let reply = serve::reply(&mut log, &message);
                if let Some(txn) = message.txn {
                    server.reply_or_drop(txn, &reply)?;
                }
            }
            Err(Error::Errno(code)) if code == -messenger::errno::ETIMEDOUT => {}
            Err(error) => return Err(error),
        }
    }
}

/// Tell `healthd` whether the journals are written, once it is up and again
/// whenever that changes (a full disk).
fn report_health(journals: &Journals, reported: &mut Option<&'static str>) {
    let (status, detail) = journals.health();
    if *reported == Some(status) {
        return;
    }
    let Ok(endpoint) = services::resolve_service(services::HEALTHD_NAME) else {
        return;
    };
    let Ok(request) = services::health_report_request("logd", status, &detail) else {
        return;
    };
    if endpoint
        .call(&request, Some(sys::clock() + HEALTH_TICKS))
        .is_ok()
    {
        *reported = Some(status);
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
