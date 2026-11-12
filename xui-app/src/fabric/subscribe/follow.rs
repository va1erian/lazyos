//! Following a topic from a UI: the one way an xui app or LazyRAD consumes
//! topics (`docs/client-api-plan.md` 3.5).
//!
//! A [`Follow`] is a handle on one topic filter. The subscription, its
//! doorbell, the drain and the reconnect all live on **one watcher thread per
//! process** that parks on the doorbells ([`Subscriber`]'s contract: woken by
//! a bell, never driven by a timer). The UI thread does no broker I/O and
//! never waits: [`Follow::take`] only empties a queue in memory.
//!
//! What wakes the UI when events were queued, in the order of preference:
//!
//! * a [`Sink<()>`] given at creation ([`Follow::with_sink`]): the window gets
//!   an ordinary app message through its proxy, so an idle loop stays parked
//!   and a busy one coalesces the rings;
//! * the follow's own doorbell ([`Follow::bell`]): a channel end to park on
//!   beside the other endpoints of a loop that is not an xui window (the
//!   windowless resident apps, `xui_app::resident`);
//! * neither: the caller takes at moments of its own (a tick it already has);
//!   that costs a mutex, not a broker call.
//!
//! The retained value of a state topic is replayed by the broker on every new
//! subscription, so a feed made early has it queued by the time it is read.

mod core;
#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use messenger_generated::topics::{self, Event, NextError, Subscription, TopicError};
use xui_work::Sink;

use self::core::{Command, Core};
use super::{Qos, Subscriber};
use crate::sys;

/// Most events a feed queues for a UI that does not read: the oldest go.
const QUEUE_CAP: usize = 512;

/// What a feed's wake does once events were queued.
pub(super) type Wake = Arc<dyn Fn() + Send + Sync>;

/// One event: its literal topic, whether it is a retained replay, and the
/// payload as the publisher's codec wrote it (the bus envelope is off).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicEvent {
    pub topic: String,
    pub retained: bool,
    /// The publisher's task slot, kernel-stamped (slots are reused).
    pub publisher: u64,
    pub payload: Vec<u8>,
}

/// The events a watcher queued for one feed.
#[derive(Default)]
pub(super) struct Queue {
    events: Mutex<VecDeque<TopicEvent>>,
    /// Whether the broker holds a subscription for the feed now.
    subscribed: AtomicBool,
}

impl Queue {
    pub(super) fn push(&self, events: Vec<TopicEvent>) {
        if events.is_empty() {
            return;
        }
        let mut queue = self.events.lock().unwrap_or_else(|e| e.into_inner());
        queue.extend(events);
        while queue.len() > QUEUE_CAP {
            queue.pop_front();
        }
    }

    pub(super) fn set_subscribed(&self, on: bool) {
        self.subscribed.store(on, Ordering::Release);
    }

    fn take(&self) -> Vec<TopicEvent> {
        let mut queue = self.events.lock().unwrap_or_else(|e| e.into_inner());
        queue.drain(..).collect()
    }
}

/// The UI side's line to the watcher thread.
struct Watcher {
    commands: Mutex<Vec<Command>>,
    /// The sending end of the control channel the thread parks on.
    control: u64,
    next_key: AtomicU64,
}

impl Watcher {
    /// Hand the watcher a command and wake it.
    fn send(&self, command: Command) {
        self.commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(command);
        ring(self.control);
    }
}

/// One message on a channel: enough to wake whoever parks on its other end. A
/// full queue already means a wake is due.
fn ring(handle: u64) {
    let _ = sys::msg_send(handle, &libmessenger::Parcel::default());
}

/// The watcher's end of a feed's doorbell, closed with the feed.
struct Post(u64);

impl Drop for Post {
    fn drop(&mut self) {
        let _ = sys::msg_close(self.0);
    }
}

/// Empty a channel's queue (the doorbell's messages carry nothing).
fn drain_bell(handle: u64) {
    let mut sink = [0u8; 256];
    while sys::msg_recv(handle, &mut sink, sys::EXPIRED_DEADLINE).is_ok() {}
}

/// The process's watcher, started by the first feed; `None` when the kernel
/// would not make its channel (no Messenger: feeds then stay empty).
fn watcher() -> Option<&'static Watcher> {
    static WATCHER: OnceLock<Option<&'static Watcher>> = OnceLock::new();
    *WATCHER.get_or_init(|| {
        let (receive, control) = sys::msg_create_pair().ok()?;
        let watcher: &'static Watcher = Box::leak(Box::new(Watcher {
            commands: Mutex::new(Vec::new()),
            control,
            next_key: AtomicU64::new(1),
        }));
        std::thread::Builder::new()
            .name(String::from("topics"))
            .spawn(move || run(watcher, receive))
            .ok()?;
        Some(watcher)
    })
}

/// The watcher thread: apply commands, subscribe what is missing, park on
/// every doorbell, read what rang.
fn run(watcher: &'static Watcher, control: u64) {
    let mut core = Core::new(Subscriber::new());
    loop {
        let commands =
            std::mem::take(&mut *watcher.commands.lock().unwrap_or_else(|e| e.into_inner()));
        for command in commands {
            core.apply(command);
        }
        core.connect(sys::clock_ticks());
        let mut handles = vec![control];
        handles.extend(core.handles());
        handles.truncate(sys::WAIT_MAX_ENDPOINTS as usize);
        let deadline = if core.busy() {
            sys::EXPIRED_DEADLINE
        } else {
            // No subscription missing: park until a bell or a command.
            core.retry_at().unwrap_or(0)
        };
        match sys::msg_wait_any(&handles, deadline) {
            Ok(ready) if ready & 1 != 0 => drain_bell(control),
            Ok(_) => {}
            Err(code) if code == -sys::errno::ETIMEDOUT => {}
            // A refused wait must not spin.
            Err(_) => sys::sleep_millis(100),
        }
        for wake in core.service(sys::clock_ticks()) {
            wake();
        }
    }
}

/// One followed topic filter (see the module docs).
pub struct Follow {
    key: u64,
    queue: Arc<Queue>,
    /// Set by the watcher when it queued events, cleared by `take`: one ring
    /// per batch.
    rung: Arc<AtomicBool>,
    /// Our end of the doorbell channel.
    bell: Option<u64>,
    /// Events [`Events::next_event`] took off the queue and not yet handed out.
    pending: VecDeque<TopicEvent>,
}

impl Follow {
    /// Follow `filter` (`+` one segment, a trailing `#` the rest) with `qos`
    /// and `depth` (`Buffered` only), and nothing to wake: the caller takes
    /// at moments of its own, or parks on [`Follow::bell`].
    pub fn new(filter: &str, qos: Qos, depth: u32) -> Follow {
        Follow::start(filter, qos, depth, None)
    }

    /// [`Follow::new`] that also rings `sink` when events were queued: a
    /// window's own wake (`Sink::bell(&proxy, Msg::Topics)`).
    pub fn with_sink(filter: &str, qos: Qos, depth: u32, sink: Sink<()>) -> Follow {
        Follow::start(filter, qos, depth, Some(sink))
    }

    fn start(filter: &str, qos: Qos, depth: u32, sink: Option<Sink<()>>) -> Follow {
        let queue = Arc::new(Queue::default());
        let rung = Arc::new(AtomicBool::new(false));
        let (bell, post) = sys::msg_create_pair().map_or((None, None), |(mine, theirs)| {
            (Some(mine), Some(Post(theirs)))
        });
        let wake: Wake = {
            let rung = Arc::clone(&rung);
            Arc::new(move || {
                if rung.swap(true, Ordering::AcqRel) {
                    return;
                }
                if let Some(post) = &post {
                    ring(post.0);
                }
                if let Some(sink) = &sink {
                    sink.ring();
                }
            })
        };
        let mut key = 0;
        if let Some(watcher) = watcher() {
            key = watcher.next_key.fetch_add(1, Ordering::Relaxed);
            watcher.send(Command::Add {
                key,
                filter: filter.to_owned(),
                qos,
                depth,
                queue: Arc::clone(&queue),
                wake,
            });
        }
        Follow {
            key,
            queue,
            rung,
            bell,
            pending: VecDeque::new(),
        }
    }

    /// The events queued since the last call, oldest first. Never waits and
    /// does no broker call.
    pub fn take(&self) -> Vec<TopicEvent> {
        // Clear the flag first: an event queued after the take below rings
        // again, one queued before it is in the queue.
        self.rung.store(false, Ordering::Release);
        if let Some(bell) = self.bell {
            drain_bell(bell);
        }
        self.queue.take()
    }

    /// Whether the watcher holds a subscription for this feed at the broker
    /// (false while the broker is away: it is made again, replay included).
    pub fn subscribed(&self) -> bool {
        self.queue.subscribed.load(Ordering::Acquire)
    }

    /// The next queued event in the shape the generated typed stream reads,
    /// kept apart from [`Follow::take`]'s: use one or the other.
    fn next_raw(&mut self) -> Option<TopicEvent> {
        if self.pending.is_empty() {
            self.pending.extend(self.take());
        }
        self.pending.pop_front()
    }

    /// The doorbell to park on (`sys::msg_wait_any`) beside other endpoints:
    /// ready when events were queued; [`Follow::take`] clears it.
    pub fn bell(&self) -> Option<u64> {
        self.bell
    }
}

impl Drop for Follow {
    fn drop(&mut self) {
        if let Some(watcher) = watcher().filter(|_| self.key != 0) {
            watcher.send(Command::Remove(self.key));
        }
        if let Some(bell) = self.bell.take() {
            let _ = sys::msg_close(bell);
        }
    }
}

/// Why a typed subscription of this runtime failed: its topic name could not
/// be built (the generated helper's own check).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FollowError(pub TopicError);

impl From<TopicError> for FollowError {
    fn from(error: TopicError) -> Self {
        FollowError(error)
    }
}

/// This runtime's [`topics::Subscribe`]: the generated `subscribe_<topic>`
/// helpers return a typed stream over a [`Follow`] when given one.
///
/// ```ignore
/// let mut stream = init_wire::subscribe_session_apps_resident(&mut Watch, &session)?;
/// while let Ok(Some(event)) = stream.next() { /* event.value: ResidentApps */ }
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct Watch;

impl topics::Subscribe for Watch {
    type Error = FollowError;
    type Subscription = Follow;

    fn subscribe_topic(&mut self, filter: &str, qos: u32) -> Result<Follow, FollowError> {
        let (qos, depth) = match qos {
            topics::QOS_BUFFERED => (Qos::Buffered, Qos::MAX_DEPTH),
            topics::QOS_CONFLATE => (Qos::Conflate, 1),
            topics::QOS_RELIABLE => (Qos::Reliable, 1),
            _ => (Qos::Latest, 1),
        };
        Ok(Follow::new(filter, qos, depth))
    }
}

impl topics::Events for Follow {
    type Error = FollowError;

    fn next_event(&mut self) -> Result<Option<topics::RawEvent>, FollowError> {
        Ok(self.next_raw().map(|event| topics::RawEvent {
            topic: event.topic,
            retained: event.retained,
            publisher: event.publisher,
            payload: event.payload,
        }))
    }
}

/// A typed stream over a [`Follow`]: what the generated `subscribe_<topic>`
/// returns for [`Watch`].
pub type Feed<T> = Subscription<Follow, T>;

/// Hand every event queued on `stream` to `apply`, oldest first. An event
/// whose payload does not decode is skipped. Reads memory only: the watcher
/// thread does the broker's calls.
pub fn pull<T>(stream: &mut Feed<T>, mut apply: impl FnMut(Event<T>)) {
    loop {
        match stream.next() {
            Ok(Some(event)) => apply(event),
            Ok(None) | Err(NextError::Transport(_)) => return,
            Err(NextError::Decode(_)) => {}
        }
    }
}
