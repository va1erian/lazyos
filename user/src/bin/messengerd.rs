//! `messengerd` (`MSGRD.ELF`): the bootstrap registry daemon (issue #89) and
//! the topics broker (issue #92).
//!
//! This is the userspace half of `docs/messenger.md` section 8. The kernel
//! owns the name table (`ipc::registry`) and publishes the bootstrap listener
//! as `os.lazy.messenger.registry`; this program claims the other end of the
//! bootstrap channel and serves requests for the life of the system.
//!
//! For names the daemon is deliberately thin: [`registry::serve_request`]
//! forwards each request to the kernel with the *requester's* task slot, so
//! the kernel opens a resolved handle straight into the requester's table and
//! records the requester as the owner of a registration. Nothing but the
//! request body and the kernel-stamped sender slot crosses this process;
//! handle numbers never have to be translated here.
//!
//! For topics the daemon is the broker. Section 20's epic decision puts
//! pub/sub in userspace first, and this file is that decision taken
//! literally: [`Broker`] owns hierarchical names, `+`/`#` filter matching,
//! QoS queues, retained values and per-subscriber drop counters, while every
//! publish and subscribe still asks the kernel's policy engine
//! (`topics_client::authorize`) before a byte is stored. Delivery is pull-based with
//! deferred replies: `NextEvent` is answered at once when an event is
//! queued, or parked (the kernel keeps the caller asleep with a real
//! deadline) until a matching publish arrives. That keeps the single-threaded
//! daemon non-blocking and the publishers free of subscriber stalls.
//!
//! The on-disk name is `MSGRD.ELF`: 8.3-safe, because the kernel's FAT
//! reader only resolves short names.
//!
//! Boot it with `LAZYOS_MESSENGERD=1` (see the kernel build script); the demo
//! then starts this program alongside `hello` and `sh`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use libmessenger::Encoder;
use user::messenger::{self, errno, registry, topics_client};
use user::sys;

/// Largest number of live subscriptions the broker keeps.
const MAX_SUBSCRIPTIONS: usize = 64;
/// Largest number of distinct topics tracked for [`Broker::list`].
const MAX_TOPICS: usize = 64;
/// Largest number of parked `NextEvent` transactions.
const MAX_PENDING: usize = 64;
/// Longest topic/filter name, mirroring the kernel ACL gate.
const MAX_NAME_BYTES: usize = 128;
/// Deepest topic/filter, mirroring the kernel ACL gate.
const MAX_SEGMENTS: usize = 8;
/// Self-soak cycles when the manifest asks for `soak` without a count.
const SOAK_DEFAULT_CYCLES: u64 = 4096;
/// Largest accepted `soak=N`, so a typo cannot run for hours.
const SOAK_MAX_CYCLES: u64 = 100_000;
/// Heap growth budget per soak cycle. The fixed 16 KiB recv-buffer leak this
/// guards against (issue #169) cost far more; the remaining ~100 bytes/cycle
/// is the encoded request/reply of the self-call itself.
const SOAK_BYTES_PER_CYCLE: u64 = 256;
/// Fixed slack on the soak's heap-growth budget, across allocator chunk
/// granularity and the other services' boot traffic during the soak.
const SOAK_BYTES_SLACK: u64 = 512 * 1024;
/// How often the daemon wakes to re-check the soak's finish conditions once
/// its cycles are done (PIT ticks).
const SOAK_IDLE_TICKS: u64 = 5;
/// Absolute-tick cap on waiting for the service topics/subs to appear after
/// the soak's cycles (PIT ticks, 100 Hz).
const SOAK_TOPICS_TICKS: u64 = 3000;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("messengerd: starting (registry daemon #89, topics broker #92)\n");
    if let Err(error) = serve() {
        sys::write_str("messengerd: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Claim the bootstrap channel, register the topics service, and serve
/// registry and topic requests forever.
fn serve() -> messenger::Result<()> {
    // The kernel holds the service end and keeps it published under the
    // well-known name; claiming the client end is what makes this task the
    // listener those resolved calls arrive at.
    let endpoint = messenger::bootstrap()?;
    sys::write_str("messengerd: bootstrap endpoint claimed\n");

    // Exercise the direct registry API once so the boot log proves the table
    // is reachable from userspace (the kernel has published one name by now).
    match registry::list() {
        Ok(entries) => sys::write_str(&format!(
            "messengerd: registry ready, {} name(s)\n",
            entries.len()
        )),
        Err(error) => {
            sys::write_str("messengerd: registry list failed: ");
            sys::write_str(error.message());
            sys::write_str("\n");
        }
    }

    // Publish the topics service name next to the registry one. Clients
    // resolve it to find the broker; `Client::connect` retries while this
    // registration is still in flight. Resolving the registry name gives this
    // task a handle to the *service* side of the bootstrap channel, which is
    // the object the new name must refer to (the same one the kernel
    // published).
    let service = registry::resolve(registry::NAME)?;
    registry::register(
        topics_client::NAME,
        &service,
        &[topics_client::INTERFACE],
        0,
    )?;
    sys::write_str("messengerd: topics service registered as ");
    sys::write_str(topics_client::NAME);
    sys::write_str("\n");

    let mut broker = Broker::new();
    // Self-soak mode (`soak=N` from the supervisor's manifest): drive N
    // request/reply cycles through this very loop and assert the daemon's bump
    // heap did not grow across them (issue #169). Off unless asked for, so a
    // plain boot serves at full speed.
    let mut soak = soak_cycles().map(Soak::start);
    if let Some(soak) = &soak {
        sys::write_str(&format!("MSGRD:SOAK:START cycles={}\n", soak.cycles_total));
    }
    // One receive buffer for the life of the daemon. The user runtime's bump
    // allocator never reclaims per-call buffers, so `Endpoint::recv`'s fresh
    // `DEFAULT_BUFFER` per message would OOM the broker after a few hundred
    // calls; `recv_with` reuses this one instead.
    let mut recv_buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    sys::write_str("messengerd: serving\n");

    loop {
        // Queue the next self-soak call. It is an ordinary call on the
        // bootstrap channel, answered by the dispatch below, so the soak
        // exercises the same path a client's poll does.
        if let Some(soak) = soak.as_mut() {
            if soak.cycles_left > 0 && soak.txn.is_none() {
                match service.begin_call(&soak.request, None) {
                    Ok(txn) => soak.txn = Some(txn),
                    Err(_) => soak.fail(),
                }
            }
        }
        let deadline = match soak.as_ref() {
            Some(soak) if soak.cycles_left == 0 => Some(sys::clock() + SOAK_IDLE_TICKS),
            _ => None,
        };
        let message = match endpoint.recv_with(&mut recv_buffer, deadline) {
            Ok(message) => message,
            // The idle wait after the soak's cycles: no message arrived, so
            // re-check whether the soak can report. Every other timeout is a
            // failure.
            Err(messenger::Error::Errno(code)) if code == -errno::ETIMEDOUT && soak.is_some() => {
                if soak.as_ref().is_some_and(|soak| soak.done(&broker)) {
                    let soak = soak.take().expect("checked above");
                    finish_soak(&broker, &soak);
                }
                continue;
            }
            Err(error) => return Err(error),
        };
        if message.interface_id() == topics_client::INTERFACE {
            serve_topic(&endpoint, &mut broker, &message);
        } else {
            let reply = match registry::serve_request(&message.parcel, message.sender) {
                Ok(reply) => reply,
                // A failed request still gets an answer, or the caller would
                // wait forever. The error reply carries the code and the
                // friendly text.
                Err(error) => registry::error_reply(message.method(), error),
            };
            // A reply can fail because the caller timed out and its
            // transaction is gone; that is a normal race, not a fatal error.
            if let Some(txn) = message.txn {
                if endpoint.reply(txn, &reply).is_err() {
                    sys::write_str("messengerd: registry reply dropped (caller gone)\n");
                }
            }
        }
        // Consume a self-soak reply with a reused buffer: `await_reply` would
        // allocate a fresh 16 KiB one per cycle, which is exactly what this
        // soak exists to keep off the serve path. The reply is already queued.
        if let Some(soak) = soak.as_mut() {
            if message.txn.is_some() && message.txn == soak.txn {
                let txn = soak.txn.take().expect("checked above");
                if await_reply_with(txn, &mut soak.reply_buffer).is_err() {
                    soak.fail();
                }
                soak.cycles_left = soak.cycles_left.saturating_sub(1);
            }
        }
        if soak.as_ref().is_some_and(|soak| soak.done(&broker)) {
            let soak = soak.take().expect("checked above");
            finish_soak(&broker, &soak);
        }
    }
}

/// The `soak=N` self-test state (issue #169): one request in flight at a
/// time, each an ordinary call the main loop serves.
struct Soak {
    /// Pre-encoded `PING` request, reused every cycle.
    request: libmessenger::Parcel,
    /// Reused `CALL_AWAIT` buffer.
    reply_buffer: Vec<u8>,
    /// Cycles requested.
    cycles_total: u64,
    /// Cycles still to run.
    cycles_left: u64,
    /// Transaction of the self-call currently being served.
    txn: Option<u64>,
    /// Heap break before the soak began.
    baseline: u64,
    /// Do not wait past this tick for the real service topics to appear.
    hard_tick: u64,
    /// A cycle failed; report `FAIL` even if the heap stayed flat.
    failed: bool,
}

impl Soak {
    /// Start a soak of `cycles` request/reply cycles and record the baseline.
    fn start(cycles: u64) -> Soak {
        Soak {
            request: topics_client::request_parcel(topics_client::method::PING, Encoder::new()),
            reply_buffer: alloc::vec![0u8; messenger::DEFAULT_BUFFER],
            cycles_total: cycles,
            cycles_left: cycles,
            txn: None,
            baseline: sys::sbrk(0),
            hard_tick: sys::clock() + SOAK_TOPICS_TICKS,
            failed: false,
        }
    }

    /// Abandon the soak (a failed cycle).
    fn fail(&mut self) {
        self.failed = true;
        self.cycles_left = 0;
        self.txn = None;
    }

    /// Whether the soak can report: its cycles are done (or failed) and either
    /// the real service topics/subscriptions are visible or the wait cap
    /// passed.
    fn done(&self, broker: &Broker) -> bool {
        if self.cycles_left > 0 {
            return false;
        }
        if self.failed {
            return true;
        }
        let (topics, subs) = broker.counts();
        (topics >= 2 && subs >= 1) || sys::clock() >= self.hard_tick
    }
}

/// Print the soak's memory verdict and the central broker's real counts.
fn finish_soak(broker: &Broker, soak: &Soak) {
    let growth = sys::sbrk(0).saturating_sub(soak.baseline);
    let budget = soak
        .cycles_total
        .saturating_mul(SOAK_BYTES_PER_CYCLE)
        .saturating_add(SOAK_BYTES_SLACK);
    if !soak.failed && growth <= budget {
        sys::write_str(&format!(
            "MSGRD:SOAK:PASS cycles={} bytes={growth}\n",
            soak.cycles_total
        ));
    } else {
        sys::write_str(&format!(
            "MSGRD:SOAK:FAIL cycles={} left={} bytes={growth} failed={}\n",
            soak.cycles_total, soak.cycles_left, soak.failed as u8
        ));
    }
    let (topics, subs) = broker.counts();
    if topics >= 2 && subs >= 1 {
        sys::write_str(&format!("MSGRD:TOPICS:PASS topics={topics} subs={subs}\n"));
    } else {
        sys::write_str(&format!("MSGRD:TOPICS:FAIL topics={topics} subs={subs}\n"));
    }
}

/// The requested self-soak cycle count: `soak=N` in the service arguments, or
/// `None` when the supervisor did not ask for one.
fn soak_cycles() -> Option<u64> {
    let mut buffer = [0u8; 128];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    for part in text.split_whitespace() {
        let Some(value) = part.strip_prefix("soak=") else {
            continue;
        };
        let cycles = value.parse::<u64>().unwrap_or(SOAK_DEFAULT_CYCLES);
        return Some(cycles.clamp(1, SOAK_MAX_CYCLES));
    }
    None
}

/// `Endpoint::await_reply` with a caller-owned buffer. The public method
/// allocates a fresh 16 KiB buffer per call, which long-lived loops must not
/// do; the soak uses this one so the daemon's own cycles stay flat too.
fn await_reply_with(txn: u64, buffer: &mut [u8]) -> messenger::Result<()> {
    let args = messenger::MsgArgs {
        txn_id: txn,
        buf_ptr: buffer.as_mut_ptr() as u64,
        buf_cap: buffer.len() as u64,
        ..messenger::MsgArgs::default()
    };
    let mut result = messenger::MsgResult::default();
    let code = sys::messenger(
        messenger::op::CALL_AWAIT,
        &args as *const messenger::MsgArgs as u64,
        &mut result as *mut messenger::MsgResult as u64,
    );
    if code < 0 {
        return Err(messenger::Error::Errno(code));
    }
    Ok(())
}

/// Handle one topics parcel: fresh events and parked-pull wakes go out first,
/// then the request's own reply (if it was not deferred).
fn serve_topic(endpoint: &messenger::Endpoint, broker: &mut Broker, message: &messenger::Message) {
    match broker.serve(&message.parcel, message.sender, message.txn) {
        Ok(outcome) => {
            // Wakes first: a parked subscriber waiting on the event this
            // request just published wakes even if the request's own reply
            // later fails. The event is popped from its queue only once the
            // reply lands, so an abandoned pull (the subscriber's poll
            // deadline expired while it waited) loses nothing: its next poll
            // takes the same event.
            for wake in outcome.wakes {
                match endpoint.reply(wake.txn, &wake.parcel) {
                    Ok(()) => broker.commit(wake.subscription, wake.sequence),
                    Err(_) => {
                        sys::write_str("messengerd: topic wake dropped (subscriber gone)\n");
                    }
                }
            }
            if let (Some(txn), Some(reply)) = (message.txn, outcome.reply) {
                match endpoint.reply(txn, &reply) {
                    Ok(()) => {
                        if let Some(delivery) = outcome.delivery {
                            broker.commit(delivery.subscription, delivery.sequence);
                        }
                    }
                    Err(_) => {
                        sys::write_str("messengerd: topic reply dropped (caller gone)\n");
                    }
                }
            }
        }
        Err(error) => {
            if let Some(txn) = message.txn {
                let reply = topics_client::error_reply(message.method(), error);
                let _ = endpoint.reply(txn, &reply);
            }
        }
    }
}

/// What one broker request produced: an optional immediate reply plus any
/// parked pulls that the request satisfied.
#[derive(Default)]
pub struct Outcome {
    reply: Option<libmessenger::Parcel>,
    wakes: Vec<Wake>,
    /// An event handed out in the request's own `NextEvent` reply; it is
    /// committed only when that reply reaches the caller.
    delivery: Option<Delivery>,
}

/// A parked pull the broker can answer.
pub struct Wake {
    txn: u64,
    parcel: libmessenger::Parcel,
    subscription: u64,
    sequence: u64,
}

/// One event handed to a subscriber; popped from its queue when the reply
/// carrying it succeeds (a failed reply means the poll was abandoned, and the
/// event must stay for the next one).
#[derive(Clone, Copy)]
struct Delivery {
    subscription: u64,
    sequence: u64,
}

/// A parsed subscription filter: literal segments plus `+` and `#` wildcards.
struct Filter {
    segments: Vec<String>,
}

impl Filter {
    /// Parse and validate; `None` for anything the ACL gate would refuse, so
    /// the broker and the kernel agree on what a valid filter is.
    fn parse(text: &str) -> Option<Filter> {
        if text.is_empty() || text.len() > MAX_NAME_BYTES {
            return None;
        }
        let segments: Vec<String> = text.split('/').map(String::from).collect();
        if segments.is_empty() || segments.len() > MAX_SEGMENTS {
            return None;
        }
        for (index, segment) in segments.iter().enumerate() {
            if !valid_segment(segment) {
                return None;
            }
            // `#` may only stand alone and only last; anywhere else it would
            // silently shadow a literal name.
            if segment.contains('#') && (segment != "#" || index + 1 != segments.len()) {
                return None;
            }
        }
        Some(Filter { segments })
    }

    /// Whether this filter matches a (literal) topic name. `+` consumes one
    /// segment; a trailing `#` consumes zero or more.
    fn matches(&self, topic: &str) -> bool {
        let topic_segments: Vec<&str> = topic.split('/').collect();
        let mut topic_index = 0;
        for segment in &self.segments {
            if segment == "#" {
                return true;
            }
            if topic_index >= topic_segments.len() {
                return false;
            }
            if segment != "+" && segment != topic_segments[topic_index] {
                return false;
            }
            topic_index += 1;
        }
        topic_index == topic_segments.len()
    }
}

/// Whether `segment` is a legal literal/wildcard segment (same byte set as the
/// kernel ACL gate in `kernel/src/ipc/topics.rs`).
fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= MAX_NAME_BYTES
        && segment.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+' | b'#')
        })
}

/// A publish topic must be literal: no wildcards (the kernel refuses them in
/// publish mode too).
fn valid_topic(topic: &str) -> bool {
    if topic.is_empty() || topic.len() > MAX_NAME_BYTES {
        return false;
    }
    let mut count = 0;
    for segment in topic.split('/') {
        if !valid_segment(segment) || segment.contains('+') || segment.contains('#') {
            return false;
        }
        count += 1;
        if count > MAX_SEGMENTS {
            return false;
        }
    }
    count > 0
}

/// Whether `topic` falls under the platform's reserved `system/` root.
fn is_system_topic(topic: &str) -> bool {
    topic == "system" || topic.starts_with("system/")
}

/// One live subscription with its QoS queue and counters.
struct Subscription {
    id: u64,
    /// Task slot the subscribing call came from (kernel-stamped).
    owner: u64,
    filter: Filter,
    qos: topics_client::Qos,
    /// Events ready for delivery (latest / buffered / reliable).
    queue: VecDeque<topics_client::Event>,
    /// Coalesced per-publisher slots (conflate only).
    conflated: Vec<(u64, topics_client::Event)>,
    drops: u64,
    delivered: u64,
    matched: u64,
}

impl Subscription {
    /// Pending events across whichever queue the QoS uses.
    fn queued(&self) -> u64 {
        (self.queue.len() + self.conflated.len()) as u64
    }
}

/// One parked `NextEvent` transaction waiting for a matching publish.
#[derive(Clone, Copy)]
struct Pending {
    subscription: u64,
    owner: u64,
    txn: u64,
}

/// A topic the broker has seen at least one publish for.
struct TopicRow {
    topic: String,
    retained: bool,
}

/// The userspace topics broker: subscriptions, retained values, fanout and
/// the drop accounting behind `docs/messenger.md` section 7.2's QoS table.
pub struct Broker {
    subscriptions: Vec<Subscription>,
    /// Retained value per topic, most recent last.
    retained: Vec<(String, topics_client::Event)>,
    topics: Vec<TopicRow>,
    pending: Vec<Pending>,
    next_subscription: u64,
    next_sequence: u64,
}

impl Default for Broker {
    /// An empty broker; ids start at 1 so 0 is never a valid handle.
    fn default() -> Broker {
        Broker {
            subscriptions: Vec::new(),
            retained: Vec::new(),
            topics: Vec::new(),
            pending: Vec::new(),
            next_subscription: 1,
            next_sequence: 1,
        }
    }
}

impl Broker {
    pub fn new() -> Broker {
        Self::default()
    }

    /// Live `(topics, subscriptions)` counts under the platform's `system/`
    /// root, for the soak evidence markers. Counting the whole table would
    /// let the `messengerctl` self-test's own `topics/`, `selftest/` traffic
    /// satisfy the soak's threshold without `sysmond`, `clipboardd` or
    /// `mimed` ever publishing centrally.
    pub fn counts(&self) -> (usize, usize) {
        let topics = self
            .topics
            .iter()
            .filter(|row| is_system_topic(&row.topic))
            .count();
        let subs = self
            .subscriptions
            .iter()
            .filter(|sub| sub.filter.segments.first().is_some_and(|s| s == "system"))
            .count();
        (topics, subs)
    }

    /// Serve one topics request from `sender` (kernel-stamped).
    pub fn serve(
        &mut self,
        request: &libmessenger::Parcel,
        sender: u64,
        txn: Option<u64>,
    ) -> Result<Outcome, messenger::Error> {
        use topics_client::{field, method, MODE_PUBLISH};

        let mut outcome = Outcome::default();
        match request.header.method {
            method::PING => {
                outcome.reply = Some(topics_client::reply_ok(request.header.method));
            }
            method::PUBLISH => {
                let topic = topics_client::string_field(request, field::TOPIC)
                    .map_err(|_| messenger::Error::Topics(errno::EINVAL))?;
                let payload = topics_client::bytes_field(request, field::PAYLOAD)
                    .map_err(|_| messenger::Error::Topics(errno::EINVAL))?
                    .ok_or(messenger::Error::Topics(errno::EINVAL))?;
                if payload.is_empty() {
                    return Err(messenger::Error::Topics(errno::EINVAL));
                }
                if payload.len() > topics_client::MAX_PAYLOAD {
                    return Err(messenger::Error::Topics(errno::E2BIG));
                }
                if !valid_topic(&topic) {
                    return Err(messenger::Error::Topics(errno::EINVAL));
                }
                let retained = topics_client::bool_field(request, field::RETAINED)
                    .map_err(|_| messenger::Error::Topics(errno::EINVAL))?;
                // Policy first: a denied publish stores nothing and is audited.
                topics_client::authorize(sender, MODE_PUBLISH, &topic, txn.unwrap_or(0))
                    .map_err(|_| messenger::Error::Topics(errno::EACCES))?;
                // `system/` is the platform's own audited namespace (service
                // status, clipboard/launch audit records, denial markers):
                // `logd` treats every event under it as authentic. The kernel
                // ACL above stays in its bootstrap-allow state until a policy
                // is loaded, so without this check any task could forge audit
                // records here. Every legitimate publisher (sysmond, clipboardd,
                // mimed, init) runs as uid 0, so gate the namespace on that.
                if is_system_topic(&topic) {
                    let mut cred = sys::Cred::default();
                    sys::cred_get(Some(sender), &mut cred)
                        .map_err(|_| messenger::Error::Topics(errno::EACCES))?;
                    if cred.uid != 0 {
                        return Err(messenger::Error::Topics(errno::EACCES));
                    }
                }
                let matched = self.publish(&topic, sender, payload, retained);
                outcome.wakes = self.satisfy();
                outcome.reply = Some(
                    topics_client::reply_matched(matched)
                        .map_err(|_| messenger::Error::Topics(errno::E2BIG))?,
                );
            }
            method::SUBSCRIBE => {
                let filter = topics_client::string_field(request, field::FILTER)
                    .map_err(|_| messenger::Error::Topics(errno::EINVAL))?;
                let qos_code = topics_client::u32_field(request, field::QOS)
                    .map_err(|_| messenger::Error::Topics(errno::EINVAL))?
                    .ok_or(messenger::Error::Topics(errno::EINVAL))?;
                let depth = topics_client::u32_field(request, field::DEPTH)
                    .map_err(|_| messenger::Error::Topics(errno::EINVAL))?
                    .unwrap_or(0);
                let qos = topics_client::Qos::from_parts(qos_code, depth)
                    .ok_or(messenger::Error::Topics(errno::EINVAL))?;
                let parsed =
                    Filter::parse(&filter).ok_or(messenger::Error::Topics(errno::EINVAL))?;
                topics_client::authorize(
                    sender,
                    topics_client::MODE_SUBSCRIBE,
                    &filter,
                    txn.unwrap_or(0),
                )
                .map_err(|_| messenger::Error::Topics(errno::EACCES))?;
                if self.subscriptions.len() >= MAX_SUBSCRIPTIONS {
                    return Err(messenger::Error::Topics(errno::ENOMEM));
                }
                let id = self.next_subscription;
                self.next_subscription += 1;
                self.subscriptions.push(Subscription {
                    id,
                    owner: sender,
                    filter: parsed,
                    qos,
                    queue: VecDeque::new(),
                    conflated: Vec::new(),
                    drops: 0,
                    delivered: 0,
                    matched: 0,
                });
                self.replay_retained(self.subscriptions.len() - 1, id);
                outcome.reply = Some(
                    topics_client::reply_subscription(id)
                        .map_err(|_| messenger::Error::Topics(errno::E2BIG))?,
                );
            }
            method::UNSUBSCRIBE => {
                let id = subscription_id(request)?;
                self.remove(id, sender)?;
                outcome.reply = Some(topics_client::reply_ok(request.header.method));
            }
            method::NEXT_EVENT => {
                let id = subscription_id(request)?;
                let index = self
                    .subscriptions
                    .iter()
                    .position(|sub| sub.id == id)
                    .ok_or(messenger::Error::Topics(errno::ENOENT))?;
                if self.subscriptions[index].owner != sender {
                    return Err(messenger::Error::Topics(errno::EPERM));
                }
                match peek(&self.subscriptions[index]) {
                    Some(event) => {
                        let sequence = event.sequence;
                        let encoded = topics_client::reply_event(event);
                        match encoded {
                            Ok(parcel) => {
                                outcome.reply = Some(parcel);
                                outcome.delivery = Some(Delivery {
                                    subscription: id,
                                    sequence,
                                });
                            }
                            Err(_) => {
                                // The event can't be encoded into a reply
                                // (e.g. too large for the buffer): drop it so
                                // a retry sees the next one instead of
                                // hitting the same unencodable head forever.
                                self.drop_undeliverable(id, sequence);
                                return Err(messenger::Error::Topics(errno::E2BIG));
                            }
                        }
                    }
                    None => {
                        let txn = txn.ok_or(messenger::Error::Topics(errno::EINVAL))?;
                        // One parked pull per (subscription, owner): the
                        // client's retry/timeout replaces its own stale entry.
                        self.pending
                            .retain(|p| !(p.subscription == id && p.owner == sender));
                        if self.pending.len() >= MAX_PENDING {
                            return Err(messenger::Error::Topics(errno::EAGAIN));
                        }
                        self.pending.push(Pending {
                            subscription: id,
                            owner: sender,
                            txn,
                        });
                        // No reply: the kernel keeps the caller parked until a
                        // publish wakes it (or its deadline expires).
                    }
                }
            }
            method::ACK => {
                let id = subscription_id(request)?;
                let sequence = topics_client::u64_field(request, field::SEQUENCE)
                    .map_err(|_| messenger::Error::Topics(errno::EINVAL))?
                    .ok_or(messenger::Error::Topics(errno::EINVAL))?;
                let index = self
                    .subscriptions
                    .iter()
                    .position(|sub| sub.id == id)
                    .ok_or(messenger::Error::Topics(errno::ENOENT))?;
                if self.subscriptions[index].owner != sender {
                    return Err(messenger::Error::Topics(errno::EPERM));
                }
                if self.subscriptions[index].qos == topics_client::Qos::Reliable {
                    let queue = &mut self.subscriptions[index].queue;
                    while let Some(front) = queue.front() {
                        if front.sequence > sequence {
                            break;
                        }
                        queue.pop_front();
                    }
                }
                outcome.reply = Some(topics_client::reply_ok(request.header.method));
            }
            method::LIST_TOPICS => {
                let list = self.list();
                outcome.reply = Some(
                    topics_client::reply_topics(&list)
                        .map_err(|_| messenger::Error::Topics(errno::E2BIG))?,
                );
            }
            method::STATS => {
                let id = subscription_id(request)?;
                let index = self
                    .subscriptions
                    .iter()
                    .position(|sub| sub.id == id)
                    .ok_or(messenger::Error::Topics(errno::ENOENT))?;
                let sub = &self.subscriptions[index];
                if sub.owner != sender {
                    return Err(messenger::Error::Topics(errno::EPERM));
                }
                let stats = topics_client::SubscriptionStats {
                    qos: sub.qos.code(),
                    depth: sub.qos.depth(),
                    queued: sub.queued(),
                    delivered: sub.delivered,
                    matched: sub.matched,
                    drops: sub.drops,
                };
                outcome.reply = Some(
                    topics_client::reply_stats(&stats)
                        .map_err(|_| messenger::Error::Topics(errno::E2BIG))?,
                );
            }
            _ => return Err(messenger::Error::Topics(errno::EINVAL)),
        }
        Ok(outcome)
    }

    /// Fan one publish out to every matching subscription, applying each
    /// subscription's QoS policy and counting drops. Returns the match count,
    /// which the publish reply reports.
    fn publish(&mut self, topic: &str, publisher: u64, payload: Vec<u8>, retained: bool) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        let event = topics_client::Event {
            topic: String::from(topic),
            publisher,
            sequence,
            retained,
            payload,
        };
        if retained {
            self.set_retained(&event);
        }
        let mut matched = 0;
        for index in 0..self.subscriptions.len() {
            if self.subscriptions[index].filter.matches(topic) {
                matched += 1;
                self.subscriptions[index].matched += 1;
                enqueue(&mut self.subscriptions[index], event.clone());
            }
        }
        self.touch_topic(topic, retained);
        matched
    }

    /// Remember (or replace) the retained value for a topic.
    fn set_retained(&mut self, event: &topics_client::Event) {
        if let Some(slot) = self
            .retained
            .iter_mut()
            .find(|(topic, _)| topic == &event.topic)
        {
            slot.1 = event.clone();
        } else if self.retained.len() < MAX_TOPICS {
            self.retained.push((event.topic.clone(), event.clone()));
        }
    }

    /// Hand the freshly created subscription any retained value its filter
    /// matches (`docs/messenger.md` 7.2: "new subscribers get it immediately").
    fn replay_retained(&mut self, index: usize, id: u64) {
        let matches: Vec<topics_client::Event> = self
            .retained
            .iter()
            .filter(|(topic, _)| self.subscriptions[index].filter.matches(topic))
            .map(|(_, event)| event.clone())
            .collect();
        let sub = &mut self.subscriptions[index];
        for mut event in matches {
            event.retained = true;
            enqueue(sub, event);
        }
        debug_assert_eq!(sub.id, id);
    }

    /// Answer every parked pull that now has a deliverable event. Events are
    /// peeked, not popped: the caller commits them once a reply reaches the
    /// subscriber (see [`Broker::commit`]).
    fn satisfy(&mut self) -> Vec<Wake> {
        let mut wakes = Vec::new();
        let mut index = 0;
        while index < self.pending.len() {
            let pending = self.pending[index];
            let Some(sub_index) = self
                .subscriptions
                .iter()
                .position(|sub| sub.id == pending.subscription && sub.owner == pending.owner)
            else {
                // The subscription is gone; drop the parked pull.
                self.pending.remove(index);
                continue;
            };
            match peek(&self.subscriptions[sub_index]) {
                Some(event) => {
                    let sequence = event.sequence;
                    let encoded = topics_client::reply_event(event);
                    self.pending.remove(index);
                    match encoded {
                        Ok(parcel) => {
                            wakes.push(Wake {
                                txn: pending.txn,
                                parcel,
                                subscription: pending.subscription,
                                sequence,
                            });
                        }
                        Err(_) => {
                            // The event can't be encoded into a reply (e.g.
                            // too large for the buffer): drop it so this
                            // subscription doesn't stall on the same
                            // unencodable head forever. This parked pull
                            // gets no reply from this round; the caller's
                            // own deadline (or its next poll) covers it.
                            self.drop_undeliverable(pending.subscription, sequence);
                        }
                    }
                }
                None => index += 1,
            }
        }
        wakes
    }

    /// Drop the head event of `id`'s queue unconditionally, including for
    /// `Reliable` (which [`Broker::commit`] otherwise never pops without an
    /// explicit `ack`), because it could not be encoded into a reply and
    /// would otherwise stall the subscription on the same event forever.
    /// Counts as a QoS drop.
    fn drop_undeliverable(&mut self, id: u64, sequence: u64) {
        let Some(sub) = self.subscriptions.iter_mut().find(|sub| sub.id == id) else {
            return;
        };
        sub.drops += 1;
        match sub.qos {
            topics_client::Qos::Conflate => {
                if sub
                    .conflated
                    .first()
                    .is_some_and(|(_, event)| event.sequence == sequence)
                {
                    sub.conflated.remove(0);
                }
            }
            topics_client::Qos::Reliable
            | topics_client::Qos::Latest
            | topics_client::Qos::Buffered(_) => {
                if sub
                    .queue
                    .front()
                    .is_some_and(|event| event.sequence == sequence)
                {
                    sub.queue.pop_front();
                }
            }
        }
    }

    /// Retire the event a successful reply carried. The head is checked by
    /// sequence, so a late commit cannot pop a newer event (`reliable`
    /// subscriptions do not pop at all; their events retire on [`ACK`]).
    /// `delivered` is counted here rather than where the reply is built,
    /// since only a reply that actually reached the subscriber (a `commit`)
    /// is a real delivery; counting earlier risked a double count when the
    /// reply failed and the same still-queued event was delivered again.
    fn commit(&mut self, id: u64, sequence: u64) {
        let Some(sub) = self.subscriptions.iter_mut().find(|sub| sub.id == id) else {
            return;
        };
        sub.delivered += 1;
        match sub.qos {
            topics_client::Qos::Reliable => {}
            topics_client::Qos::Conflate => {
                if sub
                    .conflated
                    .first()
                    .is_some_and(|(_, event)| event.sequence == sequence)
                {
                    sub.conflated.remove(0);
                }
            }
            topics_client::Qos::Latest | topics_client::Qos::Buffered(_) => {
                if sub
                    .queue
                    .front()
                    .is_some_and(|event| event.sequence == sequence)
                {
                    sub.queue.pop_front();
                }
            }
        }
    }

    /// Drop a subscription, scoped to its owner.
    fn remove(&mut self, id: u64, owner: u64) -> Result<(), messenger::Error> {
        let index = self
            .subscriptions
            .iter()
            .position(|sub| sub.id == id)
            .ok_or(messenger::Error::Topics(errno::ENOENT))?;
        if self.subscriptions[index].owner != owner {
            return Err(messenger::Error::Topics(errno::EPERM));
        }
        self.subscriptions.remove(index);
        // Parked pulls for it can never be satisfied; the clients discover
        // that on their own deadline, so just drop the bookkeeping.
        self.pending
            .retain(|pending| !(pending.subscription == id && pending.owner == owner));
        Ok(())
    }

    /// Track a topic for [`Broker::list`].
    fn touch_topic(&mut self, topic: &str, retained: bool) {
        if let Some(row) = self.topics.iter_mut().find(|row| row.topic == topic) {
            row.retained = row.retained || retained;
        } else if self.topics.len() < MAX_TOPICS {
            self.topics.push(TopicRow {
                topic: String::from(topic),
                retained,
            });
        }
    }

    /// Snapshot the topic table with live subscriber counts.
    fn list(&self) -> Vec<topics_client::TopicInfo> {
        self.topics
            .iter()
            .map(|row| topics_client::TopicInfo {
                topic: row.topic.clone(),
                subscribers: self
                    .subscriptions
                    .iter()
                    .filter(|sub| sub.filter.matches(&row.topic))
                    .count() as u64,
                retained: row.retained,
            })
            .collect()
    }
}

/// The first `SUBSCRIPTION` field of a request.
fn subscription_id(request: &libmessenger::Parcel) -> Result<u64, messenger::Error> {
    topics_client::u64_field(request, topics_client::field::SUBSCRIPTION)
        .map_err(|_| messenger::Error::Topics(errno::EINVAL))?
        .ok_or(messenger::Error::Topics(errno::EINVAL))
}

/// Queue one event under a subscription's QoS policy.
///
/// * `latest`: one slot; a new event replaces the old one (a drop).
/// * `buffered`: `depth` slots; overflow drops the oldest.
/// * `conflate`: the latest event per publisher; replacing a publisher's
///   pending event drops the coalesced one. The window is "until consumed":
///   without a clock the broker cannot time-window, and the subscriber's
///   `NextEvent` is the natural boundary.
/// * `reliable`: like buffered, but delivery does not pop; [`take`] hands out
///   the head until the subscriber acks it.
fn enqueue(sub: &mut Subscription, event: topics_client::Event) {
    match sub.qos {
        topics_client::Qos::Latest => {
            if !sub.queue.is_empty() {
                sub.queue.pop_front();
                sub.drops += 1;
            }
            sub.queue.push_back(event);
        }
        topics_client::Qos::Buffered(depth) => {
            let depth = depth.max(1) as usize;
            while sub.queue.len() >= depth {
                sub.queue.pop_front();
                sub.drops += 1;
            }
            sub.queue.push_back(event);
        }
        topics_client::Qos::Reliable => {
            let depth = topics_client::Qos::RELIABLE_DEPTH as usize;
            while sub.queue.len() >= depth {
                sub.queue.pop_front();
                sub.drops += 1;
            }
            sub.queue.push_back(event);
        }
        topics_client::Qos::Conflate => {
            if let Some(slot) = sub
                .conflated
                .iter_mut()
                .find(|(publisher, _)| *publisher == event.publisher)
            {
                *slot = (event.publisher, event);
                sub.drops += 1;
            } else {
                let window = topics_client::Qos::CONFLATE_WINDOW as usize;
                while sub.conflated.len() >= window {
                    sub.conflated.remove(0);
                    sub.drops += 1;
                }
                sub.conflated.push((event.publisher, event));
            }
        }
    }
}

/// The next event for a subscription, without consuming it: the pop happens
/// in [`Broker::commit`] once the reply carrying the event has reached the
/// subscriber. Borrowed rather than cloned: `reply_event` only needs to read
/// it, and cloning a payload-sized event on every poll is an avoidable copy.
fn peek(sub: &Subscription) -> Option<&topics_client::Event> {
    match sub.qos {
        topics_client::Qos::Conflate => sub.conflated.first().map(|(_, event)| event),
        topics_client::Qos::Reliable
        | topics_client::Qos::Latest
        | topics_client::Qos::Buffered(_) => sub.queue.front(),
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
