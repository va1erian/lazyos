//! A scripted broker for the host tests: it answers the real wire protocol
//! (decoding requests with the generated stubs) behind [`Transport`], and its
//! doorbell channels are plain queues.

use std::collections::{BTreeMap, VecDeque};

use libmessenger::{Encoder, Header, Parcel};
use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_messenger_topics_bell_v1 as bell;
use messenger_generated::os_lazy_messenger_topics_v1 as topics;

use super::{Errno, Transport, Wait, ETIMEDOUT};

/// How the fake answers a `NextEvent` that finds the queue empty.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum EmptyAs {
    /// The kernel's timeout of an expired-deadline call (what the real
    /// broker produces: it parks the pull and the caller times out).
    Timeout,
    /// A reply with an empty body.
    EmptyBody,
}

pub(super) struct FakeSub {
    pub qos: u32,
    pub depth: u32,
    pub queue: VecDeque<topics::Event>,
    /// Our (client) end of the doorbell, as the handle the client holds.
    pub bell_mine: Option<u64>,
    pub rung: bool,
    pub delivered: u64,
}

#[derive(Default)]
pub(super) struct Fake {
    pub subs: BTreeMap<u64, FakeSub>,
    next_id: u64,
    next_handle: u64,
    next_sequence: u64,
    /// Doorbell queues keyed by the client's end.
    pub bells: BTreeMap<u64, VecDeque<Vec<u8>>>,
    /// Client ends whose peer the "broker" closed.
    pub bell_dead: Vec<u64>,
    /// Handle of the broker's end -> the client's end (set by `Bell`).
    pairs: BTreeMap<u64, u64>,
    pub closed: Vec<u64>,
    pub empty_as: Option<EmptyAs>,
    /// `(method, error code)` answered once with an error field instead.
    pub fail: VecDeque<(u32, u32)>,
    /// Transport failures (`call` returns this errno) for one method, once.
    pub transport_fail: VecDeque<(u32, Errno)>,
    /// Every call answers with these raw bytes instead of a real reply.
    pub garbage: Option<Vec<u8>>,
    /// `NextEvent` polls that succeed before one fails with the errno.
    pub poll_error_after: Option<(usize, Errno)>,
    /// Every call seen: method and wait.
    pub log: Vec<(u32, Wait)>,
    pub create_pair_fail: Option<Errno>,
    /// Requests seen, as bytes, for golden checks.
    pub requests: Vec<Vec<u8>>,
}

impl Fake {
    pub fn new() -> Fake {
        Fake {
            next_id: 1,
            next_handle: 100,
            next_sequence: 1,
            ..Fake::default()
        }
    }

    /// Make the next `Subscribe` hand out `id` again (a confused broker).
    pub fn next_id_for_test(&mut self, id: u64) {
        self.next_id = id;
    }

    /// Publish `payload` on `topic` to every subscription (the tests use
    /// `#`-style filters implicitly: no filter matching is under test).
    pub fn publish(&mut self, id: u64, topic: &str, payload: &[u8]) {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        let Some(sub) = self.subs.get_mut(&id) else {
            return;
        };
        let reliable = sub.qos == topics::QOS_RELIABLE;
        let depth = if reliable {
            8
        } else if sub.qos == topics::QOS_LATEST {
            1
        } else {
            sub.depth as usize
        };
        while sub.queue.len() >= depth.max(1) {
            sub.queue.pop_front();
        }
        sub.queue.push_back(topics::Event {
            topic: topic.into(),
            publisher: 7,
            sequence,
            retained: false,
            payload: payload.to_vec(),
        });
        if let (Some(mine), false) = (sub.bell_mine, sub.rung) {
            sub.rung = true;
            let note = ready(id);
            self.bells.entry(mine).or_default().push_back(note);
        }
    }

    /// Queue `count` events at once, bypassing the depth limit (a deep
    /// broker queue for the cap tests).
    pub fn flood(&mut self, id: u64, count: usize) {
        for n in 0..count {
            let sequence = self.next_sequence;
            self.next_sequence += 1;
            if let Some(sub) = self.subs.get_mut(&id) {
                sub.queue.push_back(topics::Event {
                    topic: format!("t/{n}"),
                    publisher: 1,
                    sequence,
                    retained: n % 2 == 0,
                    payload: vec![n as u8; 3],
                });
            }
        }
    }

    fn reply(method: u32, body: Vec<u8>) -> Vec<u8> {
        let parcel = Parcel {
            header: Header {
                version: libmessenger::VERSION,
                flags: 0,
                interface_id: topics::INTERFACE_ID,
                method,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body,
            objects: Vec::new(),
        };
        let mut out = Vec::new();
        parcel.encode(&mut out).expect("fake reply encodes");
        out
    }

    fn error(method: u32, code: u32) -> Vec<u8> {
        let mut body = Encoder::new();
        body.error(ERROR_FIELD, code, "fake").unwrap();
        Self::reply(method, body.finish())
    }

    fn serve(&mut self, request: &Parcel, wait: Wait) -> Result<Vec<u8>, Errno> {
        let method = request.header.method;
        if let Some(garbage) = self.garbage.clone() {
            return Ok(garbage);
        }
        if let Some(position) = self.transport_fail.iter().position(|(m, _)| *m == method) {
            let (_, code) = self.transport_fail.remove(position).unwrap();
            return Err(code);
        }
        if let Some(position) = self.fail.iter().position(|(m, _)| *m == method) {
            let (_, code) = self.fail.remove(position).unwrap();
            return Ok(Self::error(method, code));
        }
        match method {
            topics::METHOD_SUBSCRIBE => {
                let args = topics::decode_subscribe_args(&request.body).map_err(|_| -22)?;
                let id = self.next_id;
                self.next_id += 1;
                self.subs.insert(
                    id,
                    FakeSub {
                        qos: args.qos,
                        depth: args.depth,
                        queue: VecDeque::new(),
                        bell_mine: None,
                        rung: false,
                        delivered: 0,
                    },
                );
                let body =
                    topics::encode_subscribe_reply(&topics::SubscribeReply { subscription: id });
                Ok(Self::reply(method, body.unwrap()))
            }
            topics::METHOD_UNSUBSCRIBE => {
                let args = topics::decode_unsubscribe_args(&request.body).map_err(|_| -22)?;
                match self.subs.remove(&args.subscription) {
                    Some(_) => Ok(Self::reply(method, Vec::new())),
                    None => Ok(Self::error(method, 2)),
                }
            }
            topics::METHOD_NEXTEVENT => {
                assert_eq!(wait, Wait::Poll, "a drain must never block on the broker");
                if let Some((left, code)) = self.poll_error_after {
                    if left == 0 {
                        self.poll_error_after = None;
                        return Err(code);
                    }
                    self.poll_error_after = Some((left - 1, code));
                }
                let args = topics::decode_unsubscribe_args(&request.body).map_err(|_| -22)?;
                let Some(sub) = self.subs.get_mut(&args.subscription) else {
                    return Ok(Self::error(method, 2));
                };
                let reliable = sub.qos == topics::QOS_RELIABLE;
                let head = if reliable {
                    sub.queue.front().cloned()
                } else {
                    sub.queue.pop_front()
                };
                match head {
                    Some(event) => {
                        sub.delivered += 1;
                        let body =
                            topics::encode_next_event_reply(&topics::NextEventReply { event });
                        Ok(Self::reply(method, body.unwrap()))
                    }
                    None => {
                        sub.rung = false;
                        match self.empty_as.unwrap_or(EmptyAs::Timeout) {
                            EmptyAs::Timeout => Err(-ETIMEDOUT),
                            EmptyAs::EmptyBody => Ok(Self::reply(method, Vec::new())),
                        }
                    }
                }
            }
            topics::METHOD_ACK => {
                let args = topics::decode_ack_args(&request.body).map_err(|_| -22)?;
                let Some(sub) = self.subs.get_mut(&args.subscription) else {
                    return Ok(Self::error(method, 2));
                };
                if sub.qos == topics::QOS_RELIABLE {
                    while sub
                        .queue
                        .front()
                        .is_some_and(|event| event.sequence <= args.sequence)
                    {
                        sub.queue.pop_front();
                    }
                }
                Ok(Self::reply(method, Vec::new()))
            }
            topics::METHOD_STATS => {
                let args = topics::decode_unsubscribe_args(&request.body).map_err(|_| -22)?;
                let Some(sub) = self.subs.get(&args.subscription) else {
                    return Ok(Self::error(method, 2));
                };
                let stats = topics::Stats {
                    qos: sub.qos,
                    depth: sub.depth,
                    queued: sub.queue.len() as u64,
                    delivered: sub.delivered,
                    matched: 9,
                    drops: 3,
                };
                let body = topics::encode_stats_reply(&topics::StatsReply { stats });
                Ok(Self::reply(method, body.unwrap()))
            }
            topics::METHOD_BELL => {
                let args =
                    topics::decode_bell_args(&request.body, &request.objects).map_err(|_| -22)?;
                let Some(&mine) = self.pairs.get(&args.bell) else {
                    return Ok(Self::error(method, 22));
                };
                let Some(sub) = self.subs.get_mut(&args.subscription) else {
                    return Ok(Self::error(method, 2));
                };
                sub.bell_mine = Some(mine);
                sub.rung = false;
                self.bells.entry(mine).or_default();
                Ok(Self::reply(method, Vec::new()))
            }
            _ => Ok(Self::error(method, 22)),
        }
    }
}

/// A `Ready` message for `id`, as the broker would send it.
pub(super) fn ready(id: u64) -> Vec<u8> {
    let body = bell::encode_ready_args(&bell::ReadyArgs { subscription: id }).unwrap();
    let parcel = Parcel {
        header: Header {
            version: libmessenger::VERSION,
            flags: libmessenger::flags::ONE_WAY,
            interface_id: bell::INTERFACE_ID,
            method: bell::METHOD_READY,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        objects: Vec::new(),
    };
    let mut out = Vec::new();
    parcel.encode(&mut out).unwrap();
    out
}

impl Transport for Fake {
    fn call(&mut self, request: &[u8], reply: &mut [u8], wait: Wait) -> Result<usize, Errno> {
        self.requests.push(request.to_vec());
        let parcel = Parcel::decode(request).expect("the client sends well-formed parcels");
        self.log.push((parcel.header.method, wait));
        let bytes = self.serve(&parcel, wait)?;
        let length = bytes.len().min(reply.len());
        reply[..length].copy_from_slice(&bytes[..length]);
        Ok(bytes.len())
    }

    fn create_pair(&mut self) -> Result<(u64, u64), Errno> {
        if let Some(code) = self.create_pair_fail {
            return Err(code);
        }
        let mine = self.next_handle;
        let theirs = self.next_handle + 1;
        self.next_handle += 2;
        self.pairs.insert(theirs, mine);
        Ok((mine, theirs))
    }

    fn close(&mut self, handle: u64) {
        self.closed.push(handle);
    }

    fn recv(&mut self, handle: u64, buf: &mut [u8]) -> Result<Option<usize>, Errno> {
        if let Some(message) = self.bells.get_mut(&handle).and_then(|q| q.pop_front()) {
            let length = message.len().min(buf.len());
            buf[..length].copy_from_slice(&message[..length]);
            return Ok(Some(length));
        }
        if self.bell_dead.contains(&handle) {
            return Err(-32);
        }
        Ok(None)
    }
}

/// A fake several owners can see, for tests that outlive the `Subscriber`.
#[derive(Clone)]
pub(super) struct Shared(pub std::rc::Rc<std::cell::RefCell<Fake>>);

impl Transport for Shared {
    fn call(&mut self, request: &[u8], reply: &mut [u8], wait: Wait) -> Result<usize, Errno> {
        self.0.borrow_mut().call(request, reply, wait)
    }

    fn create_pair(&mut self) -> Result<(u64, u64), Errno> {
        self.0.borrow_mut().create_pair()
    }

    fn close(&mut self, handle: u64) {
        self.0.borrow_mut().close(handle)
    }

    fn recv(&mut self, handle: u64, buf: &mut [u8]) -> Result<Option<usize>, Errno> {
        self.0.borrow_mut().recv(handle, buf)
    }
}
