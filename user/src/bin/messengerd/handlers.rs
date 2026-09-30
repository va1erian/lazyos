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
    /// Serve one topics request from `sender` (kernel-stamped).
    pub fn serve(
        &mut self,
        request: &libmessenger::Parcel,
        sender: u64,
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
/// then the request's own reply (if it was not deferred).
pub(super) fn serve_topic(
    endpoint: &messenger::Endpoint,
    broker: &mut Broker,
    message: &messenger::Message,
) {
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
                    // Expected for an abandoned poll; see the reply path below.
                    Err(_) => {}
                }
            }
            if let (Some(txn), Some(reply)) = (message.txn, outcome.reply) {
                match endpoint.reply(txn, &reply) {
                    Ok(()) => {
                        if let Some(delivery) = outcome.delivery {
                            broker.commit(delivery.subscription, delivery.sequence);
                        }
                    }
                    // Expected, not an error: a non-blocking poll
                    // (`EXPIRED_DEADLINE`) times out in the kernel before
                    // this reply is sent, and the desktop's pollers do that
                    // every tick. Nothing was committed, so the next poll
                    // takes the same event; logging it spammed once a second.
                    Err(_) => {}
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

/// The subscription a request names. Every subscription-addressed method
/// (`Unsubscribe`, `NextEvent`, `Ack`, `Stats`) puts it in field 1.
fn subscription_id(request: &libmessenger::Parcel) -> Result<u64, messenger::Error> {
    topics_client::decode_subscription_args(request)
        .map_err(|_| messenger::Error::Topics(errno::EINVAL))
}
