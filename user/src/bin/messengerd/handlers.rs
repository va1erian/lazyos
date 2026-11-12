//! Topics request handling: the broker's `Server` (policy checks, the tables)
//! behind the generated `dispatch`, the one request that parks (`NextEvent`),
//! and the endpoint-level wake/reply plumbing.

use alloc::string::String;
use alloc::vec::Vec;
use libmessenger::server::{self, Caller, Fault};
use messenger_generated::os_lazy_messenger_topics_v1 as generated;
use user::messenger::{self, errno, topics_client};
use user::sys;

use topicsbroker::{Client, Filter, Pulled, Refusal};

use super::filter::{
    holds_nic_name, is_system_topic, may_publish_system, nic_link_ifname, valid_topic,
};
use super::wire;

/// The broker's tables, ringing doorbells through Messenger endpoints.
pub(super) type Broker = topicsbroker::Broker<messenger::Endpoint>;

/// What one broker request produced: an optional immediate reply plus any
/// parked pulls that the request satisfied.
#[derive(Default)]
pub(super) struct Outcome {
    reply: Option<libmessenger::Parcel>,
    wakes: Vec<topicsbroker::Wake<libmessenger::Parcel>>,
    /// An event handed out in the request's own `NextEvent` reply; it is
    /// committed only when that reply reaches the caller.
    delivery: Option<Delivery>,
    /// A subscription this request created: rolled back when its reply
    /// cannot be delivered (the caller timed out or is gone), or the
    /// caller never learns the id and the entry leaks.
    created: Option<u64>,
}

/// One event handed to a subscriber; popped from its queue when the reply
/// carrying it succeeds (a failed reply means the poll was abandoned, and the
/// event must stay for the next one).
#[derive(Clone, Copy)]
struct Delivery {
    subscription: u64,
    sequence: u64,
}

/// A refusal with the broker's errno-style code and its friendly text.
fn refuse(code: i64) -> Fault {
    Fault::from(messenger::Error::Topics(code))
}

/// The errno-style code of a broker refusal.
fn refusal(why: Refusal) -> Fault {
    refuse(match why {
        Refusal::NoRoom => errno::ENOMEM,
        Refusal::Busy => errno::EAGAIN,
        Refusal::NotFound => errno::ENOENT,
        Refusal::NotOwner => errno::EPERM,
        Refusal::Invalid => errno::EINVAL,
    })
}

/// Who the kernel's stamp says sent a request: its slot, which alone decides
/// who may touch a subscription, and its tenant, which every budget is
/// charged to. A tenant is a platform task (no label and no login session,
/// the rule the kernel uses for the `WaitFor` reserve; the larger system
/// budget) or an app, all of whose threads and processes in one session share
/// one budget (`topicsbroker::Tenant`).
fn client_of(cx: &Caller) -> Client {
    Client::from_stamp(cx.label_id, cx.uid, cx.session, cx.sender)
}

/// [`client_of`] for a message the broker handles without the generated
/// `dispatch` (`NextEvent`, `Bell`, and the rollback of a lost reply).
fn sender_of(message: &messenger::Message) -> Client {
    let cred = message.caller();
    Client::from_stamp(cred.label_id, cred.uid, cred.session, message.sender)
}

/// Close a doorbell the broker handed back (replaced, dropped or orphaned).
fn close_bell(bell: Option<messenger::Endpoint>) {
    if let Some(bell) = bell {
        let _ = bell.close();
    }
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
    /// The index of subscription `id`, which only its creator may touch.
    fn owned(&self, id: u64, client: Client) -> Result<usize, Fault> {
        self.broker.index_of(id, client).map_err(refusal)
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
        // `ENOMEM` only for a retained value over its publisher's own budget:
        // queues shed their oldest events instead. A publish is never
        // retried, so a timeout means "maybe published" (idl/topics.midl).
        let matched = self
            .broker
            .publish(&topic, client_of(cx), payload, retained)
            .map_err(refusal)?;
        self.outcome.wakes = self.broker.satisfy(|event| wire::reply_event(event).ok());
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
        let id = self
            .broker
            .subscribe(client_of(cx), parsed, qos)
            .map_err(refusal)?;
        self.outcome.created = Some(id);
        Ok(id)
    }

    fn unsubscribe(&mut self, cx: &Caller, subscription: u64) -> Result<(), Fault> {
        close_bell(
            self.broker
                .remove(subscription, client_of(cx))
                .map_err(refusal)?,
        );
        Ok(())
    }

    /// Served by [`pull`]: it may park the caller, which a reply value cannot
    /// say.
    fn next_event(&mut self, _: &Caller, _: u64) -> Result<generated::Event, Fault> {
        Err(Fault::unserved())
    }

    fn ack(&mut self, cx: &Caller, subscription: u64, sequence: u64) -> Result<(), Fault> {
        self.broker
            .ack(subscription, client_of(cx), sequence)
            .map_err(refusal)
    }

    fn list_topics(&mut self, _: &Caller) -> Result<Vec<generated::TopicInfo>, Fault> {
        Ok(self.broker.list())
    }

    fn stats(&mut self, cx: &Caller, subscription: u64) -> Result<generated::Stats, Fault> {
        let sub = &self.broker.subscriptions[self.owned(subscription, client_of(cx))?];
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
    let sender = sender_of(message);
    // A poll (the caller's deadline already passed) is ended by the kernel
    // when this turn returns, so the broker must not park it.
    let poll = message.parcel.header.deadline_ns == libmessenger::client::POLL_HINT_NS;
    let mut outcome = Outcome::default();
    match broker
        .pull(id, sender, poll, message.txn)
        .map_err(refusal)?
    {
        Pulled::Head(index) => {
            let Some(event) = broker.subscriptions[index].peek() else {
                return Ok(outcome);
            };
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
        // No reply: for a poll the caller's turn simply ends, for a parked
        // pull the kernel keeps the caller asleep until a publish wakes it
        // (or its deadline expires).
        Pulled::Empty | Pulled::Parked => {}
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
                } else {
                    // The caller never got the id: nobody can unsubscribe.
                    close_bell(broker.settle_created(outcome.created, sender_of(message), false));
                }
            } else {
                // A subscribe sent one way has no reply to carry the id.
                close_bell(broker.settle_created(outcome.created, sender_of(message), false));
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
        Ok((id, bell)) => match broker.set_bell(id, sender_of(message), bell) {
            Ok(old) => {
                close_bell(old);
                Ok(())
            }
            Err(why) => {
                let _ = bell.close();
                Err(refusal(why))
            }
        },
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
                // The owner's end is gone: it dropped the subscription (or
                // died), and nobody can pull or unsubscribe it any more.
                close_bell(broker.reap(id));
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
