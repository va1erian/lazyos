//! The window's subscriptions: [`Subscriber`] (doorbell-driven, never polled
//! on a timer) under the portable [`Listener`] model
//! (`docs/messenger-playground-plan.md` D5).
//!
//! [`Source`] lends the subscriber to the listener as its `EventSource`. The
//! subscriber already acks a `Reliable` event as it takes it, so the source's
//! own `ack` is a no-op (a second ack would be one more broker receive).

use xui_app::fabric::{errno_text, Qos, SubId, Subscriber};
use xui_playground::listener::{BrokerStats, Config, EventSource, Listener, RawEvent, Registry};

/// The subscriber as the listener's event source.
pub struct Source<'a>(pub &'a mut Subscriber);

fn qos_of(index: u32) -> Qos {
    match index {
        0 => Qos::Latest,
        2 => Qos::Conflate,
        3 => Qos::Reliable,
        _ => Qos::Buffered,
    }
}

impl EventSource for Source<'_> {
    type Error = String;

    fn subscribe(&mut self, filter: &str, qos: u32, depth: u32) -> Result<u64, String> {
        let id = self
            .0
            .subscribe(filter, qos_of(qos), depth)
            .map_err(errno_text)?;
        // The bell is what wakes the window for this subscription.
        if let Err(code) = self.0.attach_bell(id) {
            let _ = self.0.unsubscribe(id);
            return Err(format!("doorbell: {}", errno_text(code)));
        }
        Ok(id.0)
    }

    fn next_event(&mut self, subscription: u64) -> Result<Option<RawEvent>, String> {
        let mut taken = self.0.drain(SubId(subscription), 1);
        if let Some(event) = taken.events.pop() {
            return Ok(Some(RawEvent {
                topic: event.topic,
                publisher: event.publisher,
                sequence: event.sequence,
                retained: event.retained,
                payload: event.payload,
            }));
        }
        match taken.error {
            Some(code) => Err(errno_text(code)),
            None => Ok(None),
        }
    }

    fn stats(&mut self, subscription: u64) -> Result<BrokerStats, String> {
        let s = self.0.stats(SubId(subscription)).map_err(errno_text)?;
        Ok(BrokerStats {
            qos: s.qos,
            depth: s.depth,
            queued: s.queued,
            delivered: s.delivered,
            matched: s.matched,
            drops: s.drops,
        })
    }

    fn ack(&mut self, _subscription: u64, _sequence: u64) -> Result<(), String> {
        Ok(())
    }

    fn unsubscribe(&mut self, subscription: u64) -> Result<(), String> {
        self.0.unsubscribe(SubId(subscription)).map_err(errno_text)
    }
}

/// One subscription tab.
pub struct Sub {
    pub listener: Listener,
    /// The broker's last `Stats` answer (read on demand, never on a timer).
    pub stats: Option<BrokerStats>,
    /// A drain stopped at its batch: continue on the next frame.
    pub more: bool,
    /// The last error a drain reported.
    pub error: Option<String>,
    /// The tab shows events newer than the list model has.
    pub dirty: bool,
}

/// The window's subscriptions and the one subscriber under them.
pub struct Subs {
    subscriber: Subscriber,
    pub subs: Vec<Sub>,
}

impl Default for Subs {
    fn default() -> Self {
        Subs {
            subscriber: Subscriber::new(),
            subs: Vec::new(),
        }
    }
}

impl Subs {
    /// Subscribe to `filter`; the index of the new tab.
    pub fn listen(&mut self, config: Config) -> Result<usize, String> {
        let listener = Listener::start(&mut Source(&mut self.subscriber), config)?;
        self.subs.push(Sub {
            listener,
            stats: None,
            more: false,
            error: None,
            dirty: true,
        });
        Ok(self.subs.len() - 1)
    }

    /// Index of the sub with broker id `id`.
    pub fn index_of(&self, id: SubId) -> Option<usize> {
        self.subs
            .iter()
            .position(|s| s.listener.subscription() == Some(id.0))
    }

    /// Read the doorbells (no broker call) and say which tabs have events.
    pub fn ready(&mut self) -> Vec<usize> {
        let rung = self.subscriber.consume_ready();
        let mut out: Vec<usize> = rung.ready.iter().filter_map(|id| self.index_of(*id)).collect();
        for (id, code) in rung.lost {
            if let Some(i) = self.index_of(id) {
                self.subs[i].error = Some(format!("doorbell lost: {}", errno_text(code)));
            }
        }
        // A sub whose last drain stopped at the batch has no ring coming.
        for (i, sub) in self.subs.iter().enumerate() {
            if sub.more && !out.contains(&i) {
                out.push(i);
            }
        }
        out
    }

    /// Pull ready events of tab `index` (one batch).
    pub fn drain(&mut self, index: usize, now_ms: u64, registry: &Registry) -> usize {
        let Some(sub) = self.subs.get_mut(index) else {
            return 0;
        };
        match sub
            .listener
            .drain(&mut Source(&mut self.subscriber), now_ms, registry)
        {
            Ok(drain) => {
                sub.more = drain.more;
                sub.dirty |= drain.pulled > 0;
                drain.pulled
            }
            Err(error) => {
                sub.more = false;
                sub.error = Some(error);
                0
            }
        }
    }

    /// Refresh the broker's counters of tab `index` (an explicit user action).
    pub fn read_stats(&mut self, index: usize) {
        let Some(sub) = self.subs.get_mut(index) else {
            return;
        };
        if let Some(id) = sub.listener.subscription() {
            sub.stats = Source(&mut self.subscriber).stats(id).ok();
        }
    }

    /// Close tab `index`: the subscription ends, the tab goes. When the
    /// broker refuses to drop the subscription the [`Subscriber`] keeps it
    /// (the broker's queue would leak otherwise), the error comes back, and
    /// the next close tries again; the subscriber's drop is the last try.
    pub fn close(&mut self, index: usize) -> Option<String> {
        if index >= self.subs.len() {
            return None;
        }
        self.retry_stuck();
        let mut sub = self.subs.remove(index);
        sub.listener.stop(&mut Source(&mut self.subscriber)).err()
    }

    /// Unsubscribe what an earlier close left at the broker: subscriptions
    /// the subscriber holds and no tab does.
    fn retry_stuck(&mut self) {
        let shown: Vec<u64> = self
            .subs
            .iter()
            .filter_map(|s| s.listener.subscription())
            .collect();
        for id in self.subscriber.ids() {
            if !shown.contains(&id.0) {
                let _ = self.subscriber.unsubscribe(id);
            }
        }
    }

    pub fn is_listening(&self) -> bool {
        !self.subs.is_empty()
    }
}
