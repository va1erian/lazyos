//! `logd` (`LOGD.ELF`): the structured, hash-chained event log service
//! (issue #93).
//!
//! `logd` is the S2 event log from the platform plan section 4.9. It:
//!
//! * registers [`services::LOGD_NAME`] and serves `Tail`/`Count`/`Verify` for
//!   `messengerctl log`;
//! * subscribes to the system topics: `system/events/#` on `init` (service
//!   starts/stops/crashes and login events), `system/health/+` on `healthd`
//!   (the declared retained rows, aggregate included), and `system/events/#`
//!   on `messengerd`'s central broker (the events services publish centrally:
//!   `mimed`'s launch records, the clipboard audit trail);
//! * samples the fabric audit counters and appends a
//!   `system/events/security/denial` record whenever they advance, which is
//!   the interim signal until the kernel exposes audit records to userspace;
//! * chains every record with FNV-1a over `(previous hash, seq, tick, topic,
//!   detail)`, so `Verify` detects tampering with any retained record, and
//!   keeps a bounded ring of the newest records.
//!
//! Durable output first: writing records to a persistent store is the design,
//! but the S3 writable volume does not exist yet and this branch's only
//! filesystem is the read-only FAT16 boot image, so [`WRITABLE_STORE`] is
//! `false` and the in-memory ring is used. The decision lives in one place so
//! the store plugs in where the S3 volume lands.
//!
//! On `init`'s lifecycle `Shutdown` (docs/shutdown.md) it drains its feeds a
//! last time, prints `LOGD:STOP` with the record count and the chain's
//! verdict, and exits 0; with a writable store that is where it would flush.
//!
//! Declared payloads are decoded back to their historic `key=value` text by
//! [`payload`], with the raw-bytes fallback kept for undeclared topics.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "logd/payload.rs"]
mod payload;
#[path = "logd/ring.rs"]
mod ring;

use alloc::format;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::central;
use user::messenger::services::lifecycle;
use user::messenger::{self, registry, router, services, topics_client, Error, Message, Parcel};
use user::sys;

use ring::Ring;

/// How long the service serves queries before checking its feeds again.
const POLL_TICKS: u64 = 2;
/// How often the fabric audit counters are sampled for denial records.
const DENIAL_POLL_TICKS: u64 = 25;
/// Queue depth for the central `system/events/#` audit feed. `Latest` (depth
/// one) would let the broker silently overwrite an event that arrives before
/// this loop's next poll; buffering gives the drain loop (`POLL_TICKS`
/// apart) real headroom, with any overflow still counted and logged (see
/// [`poll_central_overflow`]) rather than silently lost.
const CENTRAL_QUEUE_DEPTH: u32 = topics_client::Qos::MAX_DEPTH;
/// How often the central subscription's drop counter is sampled.
const OVERFLOW_POLL_TICKS: u64 = 25;
/// Cap on records echoed to serial, so a crash loop cannot flood the log.
const PRINT_LIMIT: u64 = 24;
/// See the module docs: no writable volume exists in this branch, so the ring
/// is the store. Flipping this to `true` (S3) sends records to the volume.
const WRITABLE_STORE: bool = false;

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
    if WRITABLE_STORE {
        sys::write_str("logd: appending to the writable store\n");
    } else {
        sys::write_str(&format!(
            "logd: no writable store; ring of {} hash-chained records\n",
            ring::RING_CAPACITY
        ));
    }
    let mut ring = Ring::new();
    let mut printed = 0u64;
    // Feeds: a cached bus endpoint plus the attached sink, so a failed
    // subscribe retries without resolving a new handle every loop.
    let mut events_bus: Option<router::Bus> = None;
    let mut events: Option<router::Subscriber> = None;
    let mut health_bus: Option<router::Bus> = None;
    let mut health: Option<router::Subscriber> = None;
    let mut central: Option<central::Bus> = None;
    let mut central_events: Option<central::Subscription> = None;
    let mut central_warned = false;
    let mut central_drops = 0u64;
    let mut audit: Option<(u64, u64, u64)> = None;
    let mut next_denial_poll = 0u64;
    let mut next_overflow_poll = 0u64;
    // Reused receive buffer: the user bump allocator never reclaims per-call
    // buffers, so the polling loop must not allocate one per message. The
    // audit snapshot buffer is reused for the same reason.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let mut stats_buffer = alloc::vec![0u8; messenger::FabricStats::SIZE];
    let mut central_stats_buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];

    loop {
        if events.is_none() {
            events_bus = connect_or_keep(events_bus, services::INIT_NAME);
            if let Some(bus) = &events_bus {
                events = bus.subscribe("system/events/#").ok();
            }
        }
        if health.is_none() {
            health_bus = connect_or_keep(health_bus, services::HEALTHD_NAME);
            if let Some(bus) = &mut health_bus {
                // The declared `system/health/{name}` pattern with its `+`
                // wildcard; it also matches the literal `summary` topic.
                health = services::health::wire::subscribe_system_health(bus, "+").ok();
            }
        }
        // Centrally published service events (`mimed` launch records, the
        // clipboard audit trail). The central broker isn't batch-subscribed
        // by the router, so this is a separate client and sink.
        if central_events.is_none() {
            if central.is_none() {
                match central::Bus::connect() {
                    Ok(bus) => central = Some(bus),
                    Err(error) => {
                        if !central_warned {
                            central_warned = true;
                            sys::write_str("logd: central broker unavailable: ");
                            sys::write_str(error.message());
                            sys::write_str("\n");
                        }
                    }
                }
            }
            if let Some(bus) = &mut central {
                match bus.subscribe_with_qos(
                    "system/events/#",
                    topics_client::Qos::Buffered(CENTRAL_QUEUE_DEPTH),
                ) {
                    Ok(subscription) => {
                        central_events = Some(subscription);
                        central_drops = 0;
                    }
                    Err(error) => {
                        // The bus itself may be the reason the subscribe
                        // failed (e.g. the broker restarted); drop it too so
                        // the next loop resolves a fresh one instead of
                        // retrying a dead handle forever.
                        central = None;
                        if !central_warned {
                            central_warned = true;
                            sys::write_str("logd: central subscribe failed: ");
                            sys::write_str(error.message());
                            sys::write_str("\n");
                        }
                    }
                }
            }
        }
        drain(&mut ring, &events, &mut printed, &mut buffer);
        drain(&mut ring, &health, &mut printed, &mut buffer);
        drain_central(
            &mut ring,
            &mut central,
            &mut central_events,
            &mut printed,
            &mut buffer,
        );

        let now = sys::clock();
        if now >= next_denial_poll {
            poll_denials(&mut ring, &mut audit, &mut printed, &mut stats_buffer);
            next_denial_poll = now + DENIAL_POLL_TICKS;
        }
        if now >= next_overflow_poll {
            poll_central_overflow(
                &mut ring,
                &central_events,
                &mut central_drops,
                &mut printed,
                &mut central_stats_buffer,
            );
            next_overflow_poll = now + OVERFLOW_POLL_TICKS;
        }

        match server.recv_with(&mut buffer, Some(sys::clock() + POLL_TICKS)) {
            Ok(message) => {
                // An orderly shutdown (docs/shutdown.md): take in what the
                // feeds still hold (the services' last stop events), then go.
                if let Some(reason) = lifecycle::stop_requested(&message) {
                    drain(&mut ring, &events, &mut printed, &mut buffer);
                    drain(&mut ring, &health, &mut printed, &mut buffer);
                    sys::write_str(&format!(
                        "LOGD:STOP records={} verified={} reason=\"{reason}\"\n",
                        ring.total,
                        ring.verify().0
                    ));
                    return Ok(());
                }
                let reply = match dispatch(&ring, &message) {
                    Ok(parcel) => parcel,
                    Err(_) => services::log_count_reply(ring.total).unwrap_or_default(),
                };
                if let Some(txn) = message.txn {
                    server.reply_or_drop(txn, &reply)?;
                }
            }
            Err(Error::Errno(code)) if code == -messenger::errno::ETIMEDOUT => {}
            Err(error) => return Err(error),
        }
    }
}

/// Reuse a cached bus, or connect once when the service appears.
fn connect_or_keep(bus: Option<router::Bus>, name: &str) -> Option<router::Bus> {
    match bus {
        Some(bus) => Some(bus),
        None => router::Bus::connect(name).ok(),
    }
}

/// Move every queued event from one subscriber into the ring.
fn drain(
    ring: &mut Ring,
    subscriber: &Option<router::Subscriber>,
    printed: &mut u64,
    buffer: &mut [u8],
) {
    let Some(subscriber) = subscriber else {
        return;
    };
    loop {
        match subscriber.recv_with(buffer, Some(messenger::EXPIRED_DEADLINE)) {
            Ok(Some(event)) => {
                let detail = payload::describe(&event.topic, &event.payload);
                let record = ring.append(sys::clock(), &event.topic, &detail);
                if *printed < PRINT_LIMIT {
                    sys::write_str(&format!(
                        "logd: record {} {} {}\n",
                        record.seq, record.topic, record.detail
                    ));
                    *printed += 1;
                }
            }
            Ok(None) => return,
            // A feed error (e.g. the broker restarted) is retried next loop.
            Err(_) => return,
        }
    }
}

/// Move every queued central-broker event into the ring. The central
/// subscription hands back the same [`router::Event`] shape as the local one,
/// so the record format is identical.
fn drain_central(
    ring: &mut Ring,
    bus: &mut Option<central::Bus>,
    subscriber: &mut Option<central::Subscription>,
    printed: &mut u64,
    buffer: &mut [u8],
) {
    let Some(sub) = subscriber.as_ref() else {
        return;
    };
    loop {
        match sub.recv_with(buffer, Some(messenger::EXPIRED_DEADLINE)) {
            Ok(Some(event)) => {
                let detail = payload::describe(&event.topic, &event.payload);
                let record = ring.append(sys::clock(), &event.topic, &detail);
                if *printed < PRINT_LIMIT {
                    sys::write_str(&format!(
                        "logd: record {} {} {}\n",
                        record.seq, record.topic, record.detail
                    ));
                    *printed += 1;
                }
            }
            Ok(None) => return,
            Err(_) => {
                // The feed died (e.g. the broker restarted): drop both the
                // subscription and the bus, or the main loop's
                // `central_events.is_none()` gate would never fire again and
                // this dead handle would be retried forever.
                *subscriber = None;
                *bus = None;
                return;
            }
        }
    }
}

/// Sample the central subscription's drop counter and append an overflow
/// record when it advances, so a `Buffered`-QoS queue that still overran
/// (a burst larger than [`CENTRAL_QUEUE_DEPTH`]) leaves its own trace in the
/// log instead of silently vanishing.
fn poll_central_overflow(
    ring: &mut Ring,
    subscriber: &Option<central::Subscription>,
    drops: &mut u64,
    printed: &mut u64,
    buffer: &mut [u8],
) {
    let Some(subscriber) = subscriber else {
        return;
    };
    let Ok(stats) = subscriber.stats_with(buffer) else {
        return;
    };
    if stats.drops <= *drops {
        return;
    }
    let delta = stats.drops - *drops;
    *drops = stats.drops;
    let detail = format!(
        "dropped={delta} total_drops={} qos={} depth={} queued={}",
        stats.drops, stats.qos, stats.depth, stats.queued
    );
    let record = ring.append(sys::clock(), "system/events/audit/overflow", &detail);
    if *printed < PRINT_LIMIT {
        sys::write_str(&format!(
            "logd: record {} {} {}\n",
            record.seq, record.topic, record.detail
        ));
        *printed += 1;
    }
}

/// Append a record when the fabric audit counters advance.
fn poll_denials(
    ring: &mut Ring,
    last: &mut Option<(u64, u64, u64)>,
    printed: &mut u64,
    stats_buffer: &mut [u8],
) {
    let Ok(stats) = messenger::fabric_stats_with(stats_buffer) else {
        return;
    };
    let counter = (stats.audit_total, stats.audit_denies, stats.audit_last_hash);
    if last.is_some_and(|previous| previous == counter) {
        return;
    }
    // The first sample only establishes a baseline unless denials already
    // happened; later samples record the delta of the fabric audit ring.
    if last.is_some() || counter.0 > 0 {
        let detail = format!(
            "audit_total={} denies={} allows={} last_hash=0x{:016x}",
            stats.audit_total, stats.audit_denies, stats.audit_allows, stats.audit_last_hash
        );
        let record = ring.append(sys::clock(), "system/events/security/denial", &detail);
        if *printed < PRINT_LIMIT {
            sys::write_str(&format!(
                "logd: record {} {} {}\n",
                record.seq, record.topic, record.detail
            ));
            *printed += 1;
        }
    }
    *last = Some(counter);
}

/// Serve one `Tail`/`Count`/`Verify` request.
fn dispatch(ring: &Ring, message: &Message) -> messenger::Result<Parcel> {
    if message.interface_id() != services::LOGD_INTERFACE {
        return Err(Error::Errno(-messenger::errno::EINVAL));
    }
    match message.method() {
        services::logd::METHOD_TAIL => {
            let count = decode_tail_count(message)?;
            let retained = ring.records();
            let start = retained.len().saturating_sub(count);
            let records: Vec<services::LogRecord> = retained[start..]
                .iter()
                .map(|record| services::LogRecord {
                    seq: record.seq,
                    tick: record.tick,
                    topic: record.topic.clone(),
                    detail: record.detail.clone(),
                    hash: record.hash,
                })
                .collect();
            services::log_records_reply(&records)
        }
        services::logd::METHOD_COUNT => services::log_count_reply(ring.total),
        services::logd::METHOD_VERIFY => {
            let (ok, index) = ring.verify();
            services::log_verify_reply(ok, index)
        }
        _ => Err(Error::Errno(-messenger::errno::EINVAL)),
    }
}

/// The `Tail` count, defaulting to 10 when the request omits it (the service's
/// historic default).
fn decode_tail_count(message: &Message) -> messenger::Result<usize> {
    let args =
        services::logd::wire::decode_tail_args(&message.parcel.body).map_err(Error::Parcel)?;
    Ok(args.count.unwrap_or(10) as usize)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
