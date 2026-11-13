//! `healthd` (`/system/bin/healthd`): service health aggregation and the retained
//! `system/health/*` topic (issue #93).
//!
//! `healthd` is the S2 health service from the platform plan section 4.3. It:
//!
//! * registers [`services::HEALTHD_NAME`] and serves `Report` (a service
//!   publishes `health/<name>` with status/detail) and `Status` (a snapshot of
//!   the retained rows plus the aggregate);
//! * reconciles `init`'s supervision table and derives a health row per
//!   service from its phase and dependency state, so a crash that `init` is
//!   restarting shows up here as `degraded` and a dependency that is not `ok`
//!   degrades its dependents;
//! * publishes every row retained on `system/health/<name>` through its
//!   [`central::Publisher`], and the aggregate on `system/health/summary`;
//! * prints one `HEALTH:SVC:PASS <name>` / `HEALTH:SVC:FAIL <name> (<status>)`
//!   serial line per transition, which is the headless evidence that the
//!   system came up and that the crash test recovered.
//!
//! Status order is `ok` < `degraded` < `down`; a reported heartbeat wins over
//! the derived row unless the derived one is worse (a service cannot report
//! itself healthy while the supervisor sees it crash).
//!
//! The supervision poll is deliberately slow and skips rows whose phase and
//! detail did not change: the user runtime's bump allocator (`user/src/heap.rs`)
//! never reclaims memory, so a per-tick reconcile would leak. The retained
//! topics are the fast path; the poll is the safety net.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "healthd/aggregate.rs"]
mod aggregate;
#[path = "healthd/feed.rs"]
mod feed;
#[path = "healthd/handler.rs"]
mod handler;

use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::central;
use user::messenger::client::Native;
use user::messenger::services::init::Client as InitClient;
use user::messenger::{self, registry, services, wait, Error};
use user::sys;

use aggregate::refresh;

/// How often the supervisor table is reconciled (PIT ticks). Service events
/// are the fast path; this poll only catches detail changes and lost events,
/// and it is slow because the user runtime's bump allocator (`user/src/heap.rs`)
/// never reclaims the reply buffers each call allocates.
const POLL_TICKS: u64 = 20_000;
/// How soon to look again while `init` cannot be reached or subscribed to
/// (it registers before it starts any service, so this is a fallback).
const CONNECT_RETRY_TICKS: u64 = 50;
/// Age at which a `Report` stops overriding the derived row (PIT ticks).
const REPORT_TTL: u64 = 250;

/// One aggregated health row.
struct HealthRow {
    name: String,
    /// Supervision phase from the last reconcile (`running`, `restarting`, ...).
    state: String,
    /// Task slot from the last reconcile.
    pid: u64,
    /// Restart count from the last reconcile.
    restarts: u64,
    /// Comma-separated dependency names from the last reconcile.
    deps: String,
    /// Aggregated status (`ok`/`degraded`/`down`).
    status: String,
    /// Human-readable detail.
    detail: String,
    /// Tick the row was last updated.
    tick: u64,
    /// Tick of the last `Report` for this row (`0` = none).
    reported_at: u64,
}

impl HealthRow {
    fn record(&self) -> services::HealthRecord {
        services::HealthRecord {
            name: self.name.clone(),
            status: self.status.clone(),
            detail: self.detail.clone(),
            tick: self.tick,
        }
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("healthd: aggregating service health (issue #93)\n");
    if let Err(error) = run() {
        sys::write_str("healthd: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Register the service and aggregate until the system ends.
fn run() -> messenger::Result<()> {
    let (published, server) = messenger::create_pair()?;
    registry::register(
        services::HEALTHD_NAME,
        &published,
        &[services::HEALTHD_INTERFACE],
        0,
    )?;
    // Serving: what waits for this service may start (init.Ready, P7.3).
    user::messenger::services::init::notify_ready();
    let mut broker = central::Publisher::new();
    let mut rows: Vec<HealthRow> = Vec::new();
    let mut init: Option<InitClient> = None;
    let mut events = feed::ServiceFeed::new();
    let mut next_poll = 0u64;
    let mut summary_state: Option<(String, String)> = None;
    // Reused receive buffers: the user bump allocator never reclaims per-call
    // buffers, so long-lived loops must not allocate one per message.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];

    loop {
        // Service state changes arrive on `init`'s retained topic; subscribe
        // as soon as the supervisor is up (the replay seeds the rows).
        // What `messengerd` could not take yet goes out once it serves.
        broker.flush();
        events.ensure();
        // A stream just made holds the retained rows: read them before parking.
        if events.fresh() {
            events.drain(false, &mut rows, &mut broker, &mut summary_state);
        }

        // Resolve the supervisor when it becomes available (logd/healthd can
        // start before it has finished registering itself).
        if init.is_none() {
            init = InitClient::try_connect(Native).ok();
        }
        let now = sys::clock();
        if now >= next_poll {
            if let Some(client) = &init {
                match client.services() {
                    Ok(statuses) => refresh(&mut rows, &statuses, &mut broker, &mut summary_state),
                    // `init` is gone: drop the handle and retry the resolution.
                    Err(_) => init = None,
                }
            }
            next_poll = sys::clock() + POLL_TICKS;
        }

        // Park on requests and `init`'s service events at once: both are
        // served as they arrive, and an idle `healthd` does not wake (P7).
        let deadline = if events.missing() || init.is_none() {
            sys::clock() + CONNECT_RETRY_TICKS
        } else {
            next_poll
        };
        let ready = match events.bell() {
            Some(bell) => wait::wait_any(&[server, bell], 0, Some(deadline)),
            None => wait::wait_any(&[server], 0, Some(deadline)),
        };
        let ready = match ready {
            Ok(ready) => ready,
            Err(Error::Errno(code)) if code == -messenger::errno::ETIMEDOUT => 0,
            Err(error) => return Err(error),
        };
        events.drain(
            ready & 0b10 != 0,
            &mut rows,
            &mut broker,
            &mut summary_state,
        );
        if ready & 1 == 0 {
            continue;
        }
        // Serve the queued message (the wait says one is there).
        match server.recv_with(&mut buffer, Some(messenger::EXPIRED_DEADLINE)) {
            Ok(message) => {
                let reply = services::health::wire::dispatch(
                    &mut handler::Handler {
                        rows: &mut rows,
                        broker: &mut broker,
                    },
                    &message,
                );
                if let Some(txn) = message.txn {
                    // A caller whose deadline passed is a normal scheduling
                    // race, not a service failure: the kernel expired the
                    // transaction, and replying to it is `-ENOENT`. Keep
                    // serving instead of taking the whole service down (a
                    // restart would also collide with the supervisor's slot
                    // reuse until registry teardown lands).
                    if let Err(error) = server.reply(txn, &reply) {
                        if error.errno() != Some(-messenger::errno::ENOENT) {
                            return Err(error);
                        }
                    }
                }
            }
            Err(Error::Errno(code)) if code == -messenger::errno::ETIMEDOUT => {}
            Err(error) => return Err(error),
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
