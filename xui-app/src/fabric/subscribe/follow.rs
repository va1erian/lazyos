//! Following a topic from a UI: the one way an xui app or LazyRAD consumes
//! topics (`docs/client-api-plan.md` 3.5).
//!
//! A [`Follow`] is a handle on one topic filter. The calling thread's hub owns
//! one [`Subscriber`] under all its follows and works to its contract
//! (woken by a doorbell, never driven by a timer):
//!
//! * a follow subscribes and attaches its doorbell when it is made (and again,
//!   at most once a second, while the broker refuses or is away);
//! * [`Follow::take`] looks at the doorbells, which are local message queues
//!   (never a call to the broker, so looking is cheap at any rate), and only
//!   when one rang drains the subscription: the broker answers a drain in its
//!   own service turn, so the UI thread never waits on it;
//! * a loop that wants to be woken parks on [`Follow::bell`] beside its other
//!   endpoints (`Resident::idle_with`); one that has a tick of its own (a
//!   window's heartbeat) just takes.
//!
//! The hub is per thread because Messenger handles and the label that lets an
//! app subscribe belong to the thread that made them: a follow is used from
//! the thread that created it. The retained value of a state topic is replayed
//! by the broker on every new subscription, so a follow made early has it by
//! the time it is read.

mod core;
#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use messenger_generated::topics::{self, Event, NextError, Subscription, TopicError};

use self::core::Core;
use super::{Qos, Subscriber, SysTransport};
use crate::sys;

/// Most events a feed queues for a UI that does not read: the oldest go.
const QUEUE_CAP: usize = 512;

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

/// The events the hub queued for one feed.
#[derive(Default)]
pub(super) struct Queue {
    events: RefCell<VecDeque<TopicEvent>>,
    /// Whether the broker holds a subscription for the feed now.
    subscribed: Cell<bool>,
}

impl Queue {
    pub(super) fn push(&self, events: Vec<TopicEvent>) {
        if events.is_empty() {
            return;
        }
        let mut queue = self.events.borrow_mut();
        queue.extend(events);
        while queue.len() > QUEUE_CAP {
            queue.pop_front();
        }
    }

    pub(super) fn set_subscribed(&self, on: bool) {
        self.subscribed.set(on);
    }

    fn take(&self) -> Vec<TopicEvent> {
        self.events.borrow_mut().drain(..).collect()
    }
}

thread_local! {
    /// This thread's subscriber and the feeds it serves.
    static HUB: RefCell<Core<SysTransport>> = RefCell::new(Core::new(Subscriber::new()));
    static NEXT_KEY: Cell<u64> = const { Cell::new(1) };
}

/// One followed topic filter (see the module docs).
pub struct Follow {
    key: u64,
    queue: Rc<Queue>,
    /// Events [`topics::Events::next_event`] took off the queue and not yet
    /// handed out.
    pending: VecDeque<TopicEvent>,
}

impl Follow {
    /// Follow `filter` (`+` one segment, a trailing `#` the rest) with `qos`
    /// and `depth` (`Buffered` only). Subscribes now, with one bounded call;
    /// a broker that is away is tried again on later takes.
    pub fn new(filter: &str, qos: Qos, depth: u32) -> Follow {
        let key = NEXT_KEY.with(|next| {
            let key = next.get();
            next.set(key + 1);
            key
        });
        let queue = Rc::new(Queue::default());
        HUB.with(|hub| {
            let mut core = hub.borrow_mut();
            core.add(key, filter, qos, depth, Rc::clone(&queue));
            core.connect(sys::clock_ticks());
        });
        Follow {
            key,
            queue,
            pending: VecDeque::new(),
        }
    }

    /// The events queued since the last call, oldest first. Looks at the
    /// doorbells (local) and, for one that rang, drains its subscription;
    /// never waits.
    pub fn take(&self) -> Vec<TopicEvent> {
        HUB.with(|hub| {
            // Not reentrant: a take from inside a take sees what is queued.
            if let Ok(mut core) = hub.try_borrow_mut() {
                let now = sys::clock_ticks();
                core.connect(now);
                core.service(now);
            }
        });
        self.queue.take()
    }

    /// Whether the broker holds a subscription for this feed (false while it
    /// is away: it is made again, replay included).
    pub fn subscribed(&self) -> bool {
        self.queue.subscribed.get()
    }

    /// The doorbell to park on (`sys::msg_wait_any`) beside other endpoints:
    /// ready when events were queued. [`Follow::take`] reads and re-arms it.
    pub fn bell(&self) -> Option<u64> {
        HUB.with(|hub| hub.try_borrow().ok()?.bell_of(self.key))
    }

    /// The next queued event in the shape the generated typed stream reads,
    /// kept apart from [`Follow::take`]'s: use one or the other.
    fn next_raw(&mut self) -> Option<TopicEvent> {
        if self.pending.is_empty() {
            self.pending.extend(self.take());
        }
        self.pending.pop_front()
    }
}

impl Drop for Follow {
    fn drop(&mut self) {
        let key = self.key;
        let _ = HUB.try_with(|hub| {
            if let Ok(mut core) = hub.try_borrow_mut() {
                core.remove(key);
            }
        });
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
/// whose payload does not decode is skipped. Never waits on the broker.
pub fn pull<T>(stream: &mut Feed<T>, mut apply: impl FnMut(Event<T>)) {
    loop {
        match stream.next() {
            Ok(Some(event)) => apply(event),
            Ok(None) | Err(NextError::Transport(_)) => return,
            Err(NextError::Decode(_)) => {}
        }
    }
}
