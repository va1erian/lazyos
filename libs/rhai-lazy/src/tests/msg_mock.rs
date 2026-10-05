//! An in-memory Messenger fabric for the `msg` tests.
//!
//! Services are closures over parcel bodies. The sample services below use
//! the *compiled* `messenger-generated` codecs, so every round trip also
//! checks that the schema-driven codec and the compiled one agree on the wire.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use messenger_generated::{
    os_lazy_confd_v1 as confd, os_lazy_echo_v1 as echo, os_lazy_messenger_topics_v1 as topics,
};

use crate::msg::{schema, Bus, BusError, Incoming, Wait};

/// One queued one-way message: `(endpoint, interface, method, body)`.
pub type Sent = (u64, u64, u32, Vec<u8>);

type Handler = Box<dyn Fn(u64, u32, &[u8]) -> Result<Vec<u8>, BusError>>;

#[derive(Default)]
pub struct MockBus {
    names: RefCell<Vec<(String, u64)>>,
    handlers: RefCell<Vec<(u64, Handler)>>,
    next: Cell<u64>,
    pub resolves: Cell<usize>,
    pub calls: Cell<usize>,
    pub sent: RefCell<Vec<Sent>>,
    /// The next call on this endpoint fails as a dead peer.
    pub kill_once: Cell<Option<u64>>,
    /// Every call times out.
    pub stalled: Cell<bool>,
    /// The fake clock; a timed-out wait advances it by its timeout.
    pub clock: Cell<u64>,
    /// Requests waiting on endpoints a script serves: `(endpoint, request)`.
    pub inbox: RefCell<VecDeque<(u64, Incoming)>>,
    /// Replies the script sent: `(txn, body)`.
    pub replies: RefCell<Vec<(u64, Vec<u8>)>>,
    /// Names registered by the script: `(name, server endpoint, interfaces)`.
    pub registered: RefCell<Vec<(String, u64, Vec<u64>)>>,
    /// Names the script withdrew: `(name, server endpoint)`.
    pub unregistered: RefCell<Vec<(String, u64)>>,
    /// How each call was allowed to wait, in order.
    pub waits: RefCell<Vec<Wait>>,
}

impl MockBus {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    /// Register `name` with a fresh endpoint served by `handler`.
    pub fn serve(
        &self,
        name: &str,
        handler: impl Fn(u64, u32, &[u8]) -> Result<Vec<u8>, BusError> + 'static,
    ) -> u64 {
        let endpoint = self.next.get() + 100;
        self.next.set(self.next.get() + 1);
        self.names.borrow_mut().push((name.to_string(), endpoint));
        self.handlers
            .borrow_mut()
            .push((endpoint, Box::new(handler)));
        endpoint
    }
}

impl Bus for MockBus {
    fn resolve(&self, name: &str) -> Result<u64, BusError> {
        self.resolves.set(self.resolves.get() + 1);
        self.names
            .borrow()
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, ep)| *ep)
            .ok_or_else(|| BusError::errno(-2))
    }

    fn call(
        &self,
        ep: u64,
        iface: u64,
        method: u32,
        body: &[u8],
        wait: Wait,
    ) -> Result<Vec<u8>, BusError> {
        let ms = wait.millis();
        self.calls.set(self.calls.get() + 1);
        self.waits.borrow_mut().push(wait);
        if self.stalled.get() {
            self.clock.set(self.clock.get() + ms);
            return Err(BusError::errno(-110));
        }
        if self.kill_once.get() == Some(ep) {
            self.kill_once.set(None);
            return Err(BusError::errno(-32));
        }
        let handlers = self.handlers.borrow();
        let (_, handler) = handlers
            .iter()
            .find(|(e, _)| *e == ep)
            .ok_or_else(|| BusError::errno(-32))?;
        let result = handler(iface, method, body);
        if matches!(&result, Err(e) if e.is_timeout()) {
            self.clock.set(self.clock.get() + ms);
        }
        result
    }

    fn send(&self, ep: u64, iface: u64, method: u32, body: &[u8]) -> Result<(), BusError> {
        self.sent
            .borrow_mut()
            .push((ep, iface, method, body.to_vec()));
        Ok(())
    }

    fn names(&self) -> Result<Vec<String>, BusError> {
        Ok(self.names.borrow().iter().map(|(n, _)| n.clone()).collect())
    }

    fn register(
        &self,
        name: &str,
        interfaces: &[u64],
        interface_names: &[&str],
    ) -> Result<u64, BusError> {
        // The kernel checks names against ids (issue #495); so does the mock.
        assert_eq!(interfaces.len(), interface_names.len());
        if self.names.borrow().iter().any(|(n, _)| n == name) {
            return Err(BusError::errno(-17));
        }
        let server = 900 + self.registered.borrow().len() as u64;
        self.names.borrow_mut().push((name.to_string(), server));
        self.registered
            .borrow_mut()
            .push((name.to_string(), server, interfaces.to_vec()));
        Ok(server)
    }

    fn recv(&self, endpoint: u64, wait: Wait) -> Result<Option<Incoming>, BusError> {
        let ms = wait.millis();
        let mut inbox = self.inbox.borrow_mut();
        match inbox.iter().position(|(ep, _)| *ep == endpoint) {
            Some(index) => Ok(inbox.remove(index).map(|(_, request)| request)),
            None => {
                self.clock.set(self.clock.get() + ms.max(1));
                Ok(None)
            }
        }
    }

    fn reply(&self, txn: u64, _iface: u64, _method: u32, body: &[u8]) -> Result<(), BusError> {
        self.replies.borrow_mut().push((txn, body.to_vec()));
        Ok(())
    }

    fn unregister(&self, name: &str, endpoint: u64) -> Result<(), BusError> {
        self.names.borrow_mut().retain(|(n, _)| n != name);
        self.unregistered
            .borrow_mut()
            .push((name.to_string(), endpoint));
        Ok(())
    }

    fn clock_ms(&self) -> u64 {
        self.clock.get()
    }
}

/// One subscription of the fake broker and its queued events.
struct Sub {
    id: u64,
    filter: String,
    queue: VecDeque<topics::Event>,
}

/// `os.lazy.messenger.topics`: subscriptions, publish fan-out and pulls, with
/// the compiled topics codec. `NextEvent` on an empty queue times out.
pub fn topics_service(bus: &MockBus) -> Rc<RefCell<Vec<u64>>> {
    let acks = Rc::new(RefCell::new(Vec::new()));
    let seen = acks.clone();
    let subs: RefCell<Vec<Sub>> = RefCell::new(Vec::new());
    let sequence = Cell::new(0u64);
    bus.serve("os.lazy.messenger.topics", move |iface, method, body| {
        assert_eq!(iface, topics::INTERFACE_ID);
        let bad = |_| BusError::errno(-22);
        match method {
            topics::METHOD_SUBSCRIBE => {
                let args = topics::decode_subscribe_args(body).map_err(bad)?;
                let id = subs.borrow().len() as u64 + 1;
                subs.borrow_mut().push(Sub {
                    id,
                    filter: args.filter,
                    queue: VecDeque::new(),
                });
                topics::encode_subscribe_reply(&topics::SubscribeReply { subscription: id })
                    .map_err(bad)
            }
            topics::METHOD_PUBLISH => {
                let args = topics::decode_publish_args(body).map_err(bad)?;
                sequence.set(sequence.get() + 1);
                let mut matched = 0;
                for sub in subs.borrow_mut().iter_mut() {
                    if schema::topic_matches(&sub.filter, &args.topic) {
                        matched += 1;
                        sub.queue.push_back(topics::Event {
                            topic: args.topic.clone(),
                            publisher: 7,
                            sequence: sequence.get(),
                            retained: args.retained,
                            payload: args.payload.clone(),
                        });
                    }
                }
                topics::encode_publish_reply(&topics::PublishReply { matched }).map_err(bad)
            }
            topics::METHOD_NEXTEVENT => {
                let args = topics::decode_next_event_args(body).map_err(bad)?;
                let mut subs = subs.borrow_mut();
                let sub = subs
                    .iter_mut()
                    .find(|s| s.id == args.subscription)
                    .ok_or(BusError::errno(-2))?;
                match sub.queue.pop_front() {
                    Some(event) => {
                        topics::encode_next_event_reply(&topics::NextEventReply { event })
                            .map_err(bad)
                    }
                    None => Err(BusError::errno(-110)),
                }
            }
            topics::METHOD_ACK => {
                let args = topics::decode_ack_args(body).map_err(bad)?;
                seen.borrow_mut().push(args.sequence);
                Ok(Vec::new())
            }
            topics::METHOD_UNSUBSCRIBE => {
                let args = topics::decode_unsubscribe_args(body).map_err(bad)?;
                subs.borrow_mut().retain(|s| s.id != args.subscription);
                Ok(Vec::new())
            }
            _ => Ok(error_reply(38, "no such method")),
        }
    });
    acks
}

fn parcel_error(_: libmessenger::Error) -> BusError {
    BusError::errno(-22)
}

/// A structured service error reply, as `confd` sends it (field 15).
pub fn error_reply(code: u32, message: &str) -> Vec<u8> {
    let mut body = libmessenger::Encoder::new();
    body.error(15, code, message).unwrap();
    body.finish()
}

/// `os.lazy.echo` replying `text` repeated `count` times; `Ping` is alive.
pub fn echo_service(bus: &MockBus) {
    bus.serve("os.lazy.echo", |iface, method, body| {
        assert_eq!(iface, echo::INTERFACE_ID, "wrong interface id on the wire");
        match method {
            echo::METHOD_ECHO => {
                let args = echo::decode_echo_args(body).map_err(parcel_error)?;
                let reply = echo::EchoReply {
                    reply: args.text.repeat(args.count as usize),
                };
                echo::encode_echo_reply(&reply).map_err(parcel_error)
            }
            echo::METHOD_PING => {
                echo::encode_ping_reply(&echo::PingReply { alive: true }).map_err(parcel_error)
            }
            _ => Ok(error_reply(38, "no such method")),
        }
    });
}

/// `os.lazy.confd` with one readable path, one denied path and `Info`.
pub fn confd_service(bus: &MockBus) {
    bus.serve("os.lazy.confd", |_, method, body| match method {
        confd::METHOD_GET => {
            let args = confd::decode_get_args(body).map_err(parcel_error)?;
            if args.path.starts_with("secret/") {
                return Ok(error_reply(13, "path is not readable by this user"));
            }
            let value = (args.path == "sys/ui/theme").then(|| confd::Value {
                kind: 3,
                str_value: Some("dark".into()),
                ..confd::Value::default()
            });
            confd::encode_get_reply(&confd::GetReply { value }).map_err(parcel_error)
        }
        confd::METHOD_SET => {
            let args = confd::decode_set_args(body).map_err(parcel_error)?;
            assert_eq!(args.value.kind, 1);
            assert_eq!(args.value.i64_value, Some(-5));
            Ok(Vec::new())
        }
        confd::METHOD_INFO => confd::encode_info_reply(&confd::InfoReply {
            store_dir: "/data/confd".into(),
            persistent: true,
        })
        .map_err(parcel_error),
        _ => Ok(error_reply(38, "no such method")),
    });
}
