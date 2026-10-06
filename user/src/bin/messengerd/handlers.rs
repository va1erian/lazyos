//! Topics request handling: the broker `serve` entry (policy checks, method
//! dispatch) and the endpoint-level wake/reply plumbing.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use user::messenger::{self, errno, topics_client};
use user::sys;

use super::broker::{
    peek, Broker, Delivery, Outcome, Pending, Subscription, MAX_PENDING, MAX_SUBSCRIPTIONS,
};
use super::filter::{is_system_topic, valid_topic, Filter};

impl Broker {
    /// Serve one topics request from the task in slot `sender`, whose
    /// credentials at send time are `caller` (both kernel-stamped).
    pub fn serve(
        &mut self,
        request: &libmessenger::Parcel,
        sender: u64,
        caller: sys::Cred,
        txn: Option<u64>,
    ) -> Result<Outcome, messenger::Error> {
        use topics_client::{method, MODE_PUBLISH};
        let invalid = |_| messenger::Error::Topics(errno::EINVAL);

        let mut outcome = Outcome::default();
        match request.header.method {
            method::PING => {
                outcome.reply = Some(topics_client::reply_ok(request.header.method));
            }
            method::PUBLISH => {
                let args = topics_client::decode_publish_args(&request.body).map_err(invalid)?;
                let (topic, payload, retained) = (args.topic, args.payload, args.retained);
                if payload.is_empty() {
                    return Err(messenger::Error::Topics(errno::EINVAL));
                }
                if payload.len() > topics_client::MAX_PAYLOAD {
                    return Err(messenger::Error::Topics(errno::E2BIG));
                }
                if !valid_topic(&topic) {
                    return Err(messenger::Error::Topics(errno::EINVAL));
                }
                // Policy first: a denied publish stores nothing and is audited.
                topics_client::authorize(sender, MODE_PUBLISH, &topic, txn.unwrap_or(0))
                    .map_err(|_| messenger::Error::Topics(errno::EACCES))?;
                // `system/` is the platform's own audited namespace (service
                // status, clipboard/launch audit records, denial markers):
                // `logd` treats every event under it as authentic. The kernel
                // ACL above stays in its bootstrap-allow state until a policy
                // is loaded, so without this check any task could forge audit
                // records here. Every legitimate publisher (sysmond, clipboardd,
                // mimed, init) runs as uid 0, so gate the namespace on that;
                // the audio driver and mixer (`_snd`, `_audio`) may publish
                // their stream events, `system/audio/...` alone (issue #453).
                if is_system_topic(&topic) {
                    let uid = caller.uid;
                    if uid != 0 && !sndpolicy::may_publish_audio_event(uid, &topic) {
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
                let args = topics_client::decode_subscribe_args(&request.body).map_err(invalid)?;
                let filter = args.filter;
                let qos = topics_client::Qos::from_parts(args.qos, args.depth)
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
                    bell: None,
                    rung: false,
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
                        // The owner drained the queue: the next event rings
                        // its bell again.
                        self.subscriptions[index].rung = false;
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
                let sequence = topics_client::decode_ack_args(&request.body)
                    .map_err(invalid)?
                    .sequence;
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
}

/// Handle one topics parcel: fresh events and parked-pull wakes go out first,
/// then the request's own reply (if it was not deferred), then the bells of
/// subscriptions left with events nobody pulled.
pub(super) fn serve_topic(
    endpoint: &messenger::Endpoint,
    broker: &mut Broker,
    message: &messenger::Message,
) {
    if message.method() == topics_client::method::BELL {
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
    match broker.serve(
        &message.parcel,
        message.sender,
        message.caller(),
        message.txn,
    ) {
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
        Err(error) => {
            if let Some(txn) = message.txn {
                let reply = topics_client::error_reply(message.method(), error);
                let _ = endpoint.reply(txn, &reply);
            }
        }
    }
}

/// `Bell`: take the transferred channel end as the subscription's doorbell.
/// A request without exactly that one handle is refused, and whatever it
/// did carry is closed, so a malformed request cannot leak handles here.
fn serve_bell(endpoint: &messenger::Endpoint, broker: &mut Broker, message: &messenger::Message) {
    let result = if message.carries(topics_client::BELL_TRANSFERS) {
        let bell = messenger::Endpoint::from_raw(message.first_handle);
        let installed = topics_client::decode_bell_args(&message.parcel)
            .map_err(|_| messenger::Error::Topics(errno::EINVAL))
            .and_then(|id| broker.set_bell(id, message.sender, bell));
        if installed.is_err() {
            let _ = bell.close();
        }
        installed
    } else {
        for offset in 0..message.handles {
            let _ = messenger::Endpoint::from_raw(message.first_handle + offset).close();
        }
        Err(messenger::Error::Topics(errno::EINVAL))
    };
    if let Some(txn) = message.txn {
        let reply = match result {
            Ok(()) => topics_client::reply_ok(message.method()),
            Err(error) => topics_client::error_reply(message.method(), error),
        };
        let _ = endpoint.reply(txn, &reply);
    }
}

/// Ring every bell that is due. A bell whose owner end is gone is dropped; a
/// full one stays rung (its owner has a `Ready` to read already).
fn ring_bells(broker: &mut Broker) {
    for (id, bell) in broker.bells_due() {
        let Ok(note) = topics_client::ready_note(id) else {
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

/// The subscription a request names. Every subscription-addressed method
/// (`Unsubscribe`, `NextEvent`, `Ack`, `Stats`) puts it in field 1.
fn subscription_id(request: &libmessenger::Parcel) -> Result<u64, messenger::Error> {
    topics_client::decode_subscription_args(request)
        .map_err(|_| messenger::Error::Topics(errno::EINVAL))
}
