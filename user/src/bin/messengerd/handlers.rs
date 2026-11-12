//! Topics request handling: the broker's `Server` (policy checks, the tables)
//! behind the generated `dispatch`, the one request that parks (`NextEvent`),
//! and the endpoint-level wake/reply plumbing.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use libmessenger::server::{self, Caller, Fault};
use messenger_generated::os_lazy_messenger_topics_v1 as generated;
use user::messenger::{self, errno, topics_client};
use user::sys;

use super::broker::{
    peek, Broker, Delivery, Outcome, Pending, Subscription, MAX_PENDING, MAX_SUBSCRIPTIONS,
};
use super::filter::{
    holds_nic_name, is_system_topic, may_publish_system, nic_link_ifname, valid_topic, Filter,
};
use super::wire;

/// A refusal with the broker's errno-style code and its friendly text.
fn refuse(code: i64) -> Fault {
    Fault::from(messenger::Error::Topics(code))
}

/// One request being served: the broker's tables, and what the request
/// produced besides its reply (parked pulls it satisfied).
struct Request<'a> {
    broker: &'a mut Broker,
    outcome: Outcome,
    /// The request's transaction, copied into the kernel's denial records.
    txn: Option<u64>,
}

impl Request<'_> {
    /// The index of subscription `id`, which only its `owner` may touch.
    fn owned(&self, id: u64, owner: u64) -> Result<usize, Fault> {
        let index = self
            .broker
            .subscriptions
            .iter()
            .position(|sub| sub.id == id)
            .ok_or_else(|| refuse(errno::ENOENT))?;
        if self.broker.subscriptions[index].owner != owner {
            return Err(refuse(errno::EPERM));
        }
        Ok(index)
    }
}

/// Whether `cx` may publish `topic` under the platform's `system/` namespace
/// (anything else is the kernel ACL's business alone).
///
/// `system/` is the platform's own audited record (service status,
/// clipboard/launch audit records, denial markers): `logd` treats every event
/// under it as authentic. The kernel ACL stays in its bootstrap-allow state
/// until a policy is loaded, so without this check any task could forge audit
/// records here. The platform publishers (sysmond, clipboardd, mimed, init)
/// run as uid 0; a dedicated system uid gets its own subtree only
/// (`may_publish_system`), and the audio driver and mixer (`_snd`, `_audio`)
/// their stream events, `system/audio/...` alone (issue #453).
fn may_publish(cx: &Caller, topic: &str) -> bool {
    if !is_system_topic(topic) {
        return true;
    }
    if !may_publish_system(topic, cx.uid) && !sndpolicy::may_publish_audio_event(cx.uid, topic) {
        return false;
    }
    // A driver uid speaks for the card it serves, not for every card (root is
    // the console's boot identity).
    match nic_link_ifname(topic) {
        Some(ifname) => cx.uid == 0 || holds_nic_name(cx.sender, ifname),
        None => true,
    }
}

impl generated::Server for Request<'_> {
    fn publish(
        &mut self,
        cx: &Caller,
        topic: String,
        payload: Vec<u8>,
        retained: bool,
    ) -> Result<u64, Fault> {
        if payload.is_empty() || !valid_topic(&topic) {
            return Err(refuse(errno::EINVAL));
        }
        if payload.len() > topics_client::MAX_PAYLOAD {
            return Err(refuse(errno::E2BIG));
        }
        // Policy first: a denied publish stores nothing and is audited.
        let txn = self.txn.unwrap_or(0);
        topics_client::authorize(cx.sender, topics_client::MODE_PUBLISH, &topic, txn)
            .map_err(|_| refuse(errno::EACCES))?;
        if !may_publish(cx, &topic) {
            return Err(refuse(errno::EACCES));
        }
        let matched = self.broker.publish(&topic, cx.sender, payload, retained);
        self.outcome.wakes = self.broker.satisfy();
        Ok(matched)
    }

    fn subscribe(
        &mut self,
        cx: &Caller,
        filter: String,
        qos: u32,
        depth: u32,
    ) -> Result<u64, Fault> {
        let qos =
            topics_client::Qos::from_parts(qos, depth).ok_or_else(|| refuse(errno::EINVAL))?;
        let parsed = Filter::parse(&filter).ok_or_else(|| refuse(errno::EINVAL))?;
        let txn = self.txn.unwrap_or(0);
        topics_client::authorize(cx.sender, topics_client::MODE_SUBSCRIBE, &filter, txn)
            .map_err(|_| refuse(errno::EACCES))?;
        let broker = &mut *self.broker;
        if broker.subscriptions.len() >= MAX_SUBSCRIPTIONS {
            return Err(refuse(errno::ENOMEM));
        }
        let id = broker.next_subscription;
        broker.next_subscription += 1;
        broker.subscriptions.push(Subscription {
            id,
            owner: cx.sender,
            filter: parsed,
            qos,
            queue: VecDeque::new(),
            conflated: Vec::new(),
            drops: 0,
            delivered: 0,
            matched: 0,
            bell: None,
            rung: false,
        });
        broker.replay_retained(broker.subscriptions.len() - 1, id);
        Ok(id)
    }

    fn unsubscribe(&mut self, cx: &Caller, subscription: u64) -> Result<(), Fault> {
        Ok(self.broker.remove(subscription, cx.sender)?)
    }

    /// Served by [`pull`]: it may park the caller, which a reply value cannot
    /// say.
    fn next_event(&mut self, _: &Caller, _: u64) -> Result<generated::Event, Fault> {
        Err(Fault::unserved())
    }

    fn ack(&mut self, cx: &Caller, subscription: u64, sequence: u64) -> Result<(), Fault> {
        let index = self.owned(subscription, cx.sender)?;
        let sub = &mut self.broker.subscriptions[index];
        if sub.qos == topics_client::Qos::Reliable {
            while sub
                .queue
                .front()
                .is_some_and(|front| front.sequence <= sequence)
            {
                sub.queue.pop_front();
            }
        }
        Ok(())
    }

    fn list_topics(&mut self, _: &Caller) -> Result<Vec<generated::TopicInfo>, Fault> {
        Ok(self.broker.list())
    }

    fn stats(&mut self, cx: &Caller, subscription: u64) -> Result<generated::Stats, Fault> {
        let sub = &self.broker.subscriptions[self.owned(subscription, cx.sender)?];
        Ok(generated::Stats {
            qos: sub.qos.code(),
            depth: sub.qos.depth(),
            queued: sub.queued(),
            delivered: sub.delivered,
            matched: sub.matched,
            drops: sub.drops,
        })
    }

    fn ping(&mut self, _: &Caller) -> Result<(), Fault> {
        Ok(())
    }

    /// Served by [`serve_bell`]: the request carries a channel end.
    fn bell(&mut self, _: &Caller, _: u64, _: u64) -> Result<(), Fault> {
        Err(Fault::unserved())
    }
}

/// `NextEvent`: answer with the subscription's head event (committed once the
/// reply reaches the caller), or park the caller until a publish wakes it.
fn pull(broker: &mut Broker, message: &messenger::Message) -> Result<Outcome, Fault> {
    let id = wire::subscription_id(&message.parcel).map_err(Fault::from)?;
    let sender = message.sender;
    let index = broker
        .subscriptions
        .iter()
        .position(|sub| sub.id == id)
        .ok_or_else(|| refuse(errno::ENOENT))?;
    if broker.subscriptions[index].owner != sender {
        return Err(refuse(errno::EPERM));
    }
    let mut outcome = Outcome::default();
    match peek(&broker.subscriptions[index]) {
        Some(event) => {
            let sequence = event.sequence;
            match wire::reply_event(event) {
                Ok(parcel) => {
                    outcome.reply = Some(parcel);
                    outcome.delivery = Some(Delivery {
                        subscription: id,
                        sequence,
                    });
                }
                Err(_) => {
                    // The event can't be encoded into a reply (e.g. too large
                    // for the buffer): drop it so a retry sees the next one
                    // instead of hitting the same unencodable head forever.
                    broker.drop_undeliverable(id, sequence);
                    return Err(refuse(errno::E2BIG));
                }
            }
        }
        None => {
            // The owner drained the queue: the next event rings its bell
            // again.
            broker.subscriptions[index].rung = false;
            let txn = message.txn.ok_or_else(|| refuse(errno::EINVAL))?;
            // One parked pull per (subscription, owner): the client's
            // retry/timeout replaces its own stale entry.
            broker
                .pending
                .retain(|p| !(p.subscription == id && p.owner == sender));
            if broker.pending.len() >= MAX_PENDING {
                return Err(refuse(errno::EAGAIN));
            }
            broker.pending.push(Pending {
                subscription: id,
                owner: sender,
                txn,
            });
            // No reply: the kernel keeps the caller parked until a publish
            // wakes it (or its deadline expires).
        }
    }
    Ok(outcome)
}

/// Handle one topics parcel: fresh events and parked-pull wakes go out first,
/// then the request's own reply (if it was not deferred), then the bells of
/// subscriptions left with events nobody pulled.
pub(super) fn serve_topic(
    endpoint: &messenger::Endpoint,
    broker: &mut Broker,
    message: &messenger::Message,
) {
    if message.method() == generated::METHOD_BELL {
        serve_bell(endpoint, broker, message);
    } else {
        serve_request(endpoint, broker, message);
    }
    ring_bells(broker);
}

fn serve_request(
    endpoint: &messenger::Endpoint,
    broker: &mut Broker,
    message: &messenger::Message,
) {
    let served = if message.method() == generated::METHOD_NEXTEVENT {
        pull(broker, message)
    } else {
        let mut request = Request {
            broker,
            outcome: Outcome::default(),
            txn: message.txn,
        };
        let reply = generated::dispatch(&mut request, message);
        Ok(Outcome {
            reply: Some(reply),
            ..request.outcome
        })
    };
    match served {
        Ok(outcome) => {
            // Wakes first: a parked subscriber waiting on the event this
            // request just published wakes even if the request's own reply
            // later fails. The event is popped from its queue only once the
            // reply lands, so an abandoned pull (the subscriber's poll
            // deadline expired while it waited) loses nothing: its next poll
            // takes the same event.
            for wake in outcome.wakes {
                if delivered(endpoint, wake.txn, &wake.parcel, "topic wake") {
                    broker.commit(wake.subscription, wake.sequence);
                }
            }
            if let (Some(txn), Some(reply)) = (message.txn, outcome.reply) {
                if delivered(endpoint, txn, &reply, "topic reply") {
                    if let Some(delivery) = outcome.delivery {
                        broker.commit(delivery.subscription, delivery.sequence);
                    }
                }
            }
        }
        Err(fault) => {
            if let Some(txn) = message.txn {
                let reply = server::fault_reply(topics_client::INTERFACE, message.method(), &fault);
                let _ = endpoint.reply(txn, &reply);
            }
        }
    }
}

/// `Bell`: take the channel end the request carried as the subscription's
/// doorbell. A malformed request leaves the end with the message, which
/// closes it, so nothing can leak handles here.
fn serve_bell(endpoint: &messenger::Endpoint, broker: &mut Broker, message: &messenger::Message) {
    let result = match wire::decode_bell_args(message) {
        Ok((id, bell)) => {
            let installed = broker.set_bell(id, message.sender, bell);
            if installed.is_err() {
                let _ = bell.close();
            }
            installed.map_err(Fault::from)
        }
        Err(_) => Err(refuse(errno::EINVAL)),
    };
    if let Some(txn) = message.txn {
        let interface = topics_client::INTERFACE;
        let reply = server::finish(interface, message.method(), result, |()| Ok(Vec::new()));
        let _ = endpoint.reply(txn, &reply);
    }
}

/// Ring every bell that is due. A bell whose owner end is gone is dropped; a
/// full one stays rung (its owner has a `Ready` to read already).
fn ring_bells(broker: &mut Broker) {
    for (id, bell) in broker.bells_due() {
        let Ok(note) = wire::ready_note(id) else {
            continue;
        };
        if let Err(error) = bell.send(&note) {
            if error.errno() == Some(-errno::EPIPE) {
                broker.drop_bell(id);
            }
        }
    }
}

/// Send `parcel` as the reply to `txn`; `true` only if the caller received it.
///
/// A vanished caller (`-ENOENT`: deadline passed, canceled, exited) is an
/// ordinary race, not a fault: a non-blocking poll (`EXPIRED_DEADLINE`) times
/// out in the kernel before the reply is sent, and the desktop's pollers do
/// that every tick. It is reported as `false` without logging, so the caller
/// skips the broker commit and the next poll takes the same event. Any other
/// error is unexpected and logged under `what`.
fn delivered(
    endpoint: &messenger::Endpoint,
    txn: u64,
    parcel: &libmessenger::Parcel,
    what: &str,
) -> bool {
    match endpoint.reply(txn, parcel) {
        Ok(()) => true,
        Err(error) => {
            if error.errno() != Some(-errno::ENOENT) {
                sys::write_str("messengerd: ");
                sys::write_str(what);
                sys::write_str(
                    " dropped (reply failed)
",
                );
            }
            false
        }
    }
}
