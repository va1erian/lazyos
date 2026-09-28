//! `logd` (`LOGD.ELF`): the structured, hash-chained event log service
//! (issue #93).
//!
//! `logd` is the S2 event log from the platform plan section 4.9. It:
//!
//! * registers [`services::LOGD_NAME`] and serves `Tail`/`Count`/`Verify` for
//!   `messengerctl log`;
//! * subscribes to the system topics through the userspace router:
//!   `system/events/#` on `init` (service starts/stops/crashes) and
//!   `system/health/#` on `healthd` (retained health rows);
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
//! Logins join the subscription list as soon as `logind` exists; their topic
//! prefix is reserved (`system/events/login/#`) but nothing publishes it yet.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::messenger::{self, registry, router, services, Error, Message, Parcel};
use user::sys;

/// Newest records kept in the ring.
const RING_CAPACITY: usize = 64;
/// How long the service serves queries before checking its feeds again.
const POLL_TICKS: u64 = 2;
/// How often the fabric audit counters are sampled for denial records.
const DENIAL_POLL_TICKS: u64 = 25;
/// Cap on records echoed to serial, so a crash loop cannot flood the log.
const PRINT_LIMIT: u64 = 24;
/// See the module docs: no writable volume exists in this branch, so the ring
/// is the store. Flipping this to `true` (S3) sends records to the volume.
const WRITABLE_STORE: bool = false;

/// One hash-chained log record.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    seq: u64,
    tick: u64,
    topic: String,
    detail: String,
    hash: u64,
}

/// The bounded ring and its chain head.
struct Ring {
    records: Vec<Record>,
    /// Hash of the newest record (`0` before the first append).
    head: u64,
    /// Records appended since boot.
    total: u64,
    /// Oldest records dropped when the ring wrapped.
    dropped: u64,
}

impl Ring {
    fn new() -> Ring {
        Ring {
            records: Vec::new(),
            head: 0,
            total: 0,
            dropped: 0,
        }
    }

    /// Append a record, extending the chain; returns the new record.
    fn append(&mut self, tick: u64, topic: &str, detail: &str) -> Record {
        let seq = self.total + 1;
        let hash = record_hash(self.head, seq, tick, topic, detail);
        if self.records.len() >= RING_CAPACITY {
            self.records.remove(0);
            self.dropped += 1;
        }
        let record = Record {
            seq,
            tick,
            topic: topic.to_string(),
            detail: detail.to_string(),
            hash,
        };
        self.records.push(record.clone());
        self.head = hash;
        self.total = seq;
        record
    }

    /// Verify the retained window's chain links, and the genesis link while
    /// nothing has been dropped. Returns `(intact, first bad index)`; the index
    /// is the record count when the chain is intact.
    fn verify(&self) -> (bool, u64) {
        let mut previous = 0u64;
        for (index, record) in self.records.iter().enumerate() {
            let expected = record_hash(
                previous,
                record.seq,
                record.tick,
                &record.topic,
                &record.detail,
            );
            // The first retained record's predecessor may have been dropped;
            // the window then starts trusted and every following link is
            // still checked.
            if record.hash != expected && !(index == 0 && self.dropped > 0) {
                return (false, index as u64);
            }
            previous = record.hash;
        }
        (true, self.records.len() as u64)
    }
}

/// FNV-1a over the previous hash and the record fields.
fn record_hash(previous: u64, seq: u64, tick: u64, topic: &str, detail: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = if previous == 0 { OFFSET } else { previous };
    for byte in seq
        .to_le_bytes()
        .iter()
        .chain(tick.to_le_bytes().iter())
        .chain(topic.as_bytes())
        .chain(b"|")
        .chain(detail.as_bytes())
    {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

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
        &[services::LOGD_INTERFACE],
        0,
    )?;
    if WRITABLE_STORE {
        sys::write_str("logd: appending to the writable store\n");
    } else {
        sys::write_str(&format!(
            "logd: no writable store; ring of {RING_CAPACITY} hash-chained records\n"
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
    let mut audit: Option<(u64, u64, u64)> = None;
    let mut next_denial_poll = 0u64;
    // Reused receive buffer: the user bump allocator never reclaims per-call
    // buffers, so the polling loop must not allocate one per message. The
    // audit snapshot buffer is reused for the same reason.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let mut stats_buffer = alloc::vec![0u8; messenger::FabricStats::SIZE];

    loop {
        if events.is_none() {
            events_bus = connect_or_keep(events_bus, services::INIT_NAME);
            if let Some(bus) = &events_bus {
                events = bus.subscribe("system/events/#").ok();
            }
        }
        if health.is_none() {
            health_bus = connect_or_keep(health_bus, services::HEALTHD_NAME);
            if let Some(bus) = &health_bus {
                health = bus.subscribe("system/health/#").ok();
            }
        }
        drain(&mut ring, &events, &mut printed, &mut buffer);
        drain(&mut ring, &health, &mut printed, &mut buffer);

        let now = sys::clock();
        if now >= next_denial_poll {
            poll_denials(&mut ring, &mut audit, &mut printed, &mut stats_buffer);
            next_denial_poll = now + DENIAL_POLL_TICKS;
        }

        match server.recv_with(&mut buffer, Some(sys::clock() + POLL_TICKS)) {
            Ok(message) => {
                let reply = match dispatch(&ring, &message) {
                    Ok(parcel) => parcel,
                    Err(_) => services::log_count_reply(ring.total).unwrap_or_default(),
                };
                if let Some(txn) = message.txn {
                    server.reply(txn, &reply)?;
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
                let detail = core::str::from_utf8(&event.payload).unwrap_or("<binary>");
                let record = ring.append(sys::clock(), &event.topic, detail);
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
        services::logd_method::TAIL => {
            let count = message_u64(message, services::field::COUNT).unwrap_or(10) as usize;
            let start = ring.records.len().saturating_sub(count);
            let records: Vec<services::LogRecord> = ring.records[start..]
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
        services::logd_method::COUNT => services::log_count_reply(ring.total),
        services::logd_method::VERIFY => {
            let (ok, index) = ring.verify();
            services::log_verify_reply(ok, index)
        }
        _ => Err(Error::Errno(-messenger::errno::EINVAL)),
    }
}

/// The first `u64` field with the given id in a message body.
fn message_u64(message: &Message, id: u16) -> Option<u64> {
    use libmessenger::{Decoder, Kind};
    let mut decoder = Decoder::new(&message.parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::U64 && field.id == id {
            return field.as_u64().ok();
        }
    }
    None
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
