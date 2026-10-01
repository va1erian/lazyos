//! An in-memory Messenger fabric for the `msg` tests.
//!
//! Services are closures over parcel bodies. The sample services below use
//! the *compiled* `messenger-generated` codecs, so every round trip also
//! checks that the schema-driven codec and the compiled one agree on the wire.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use messenger_generated::{os_lazy_confd_v1 as confd, os_lazy_echo_v1 as echo};

use crate::msg::{Bus, BusError};

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
        _ms: u64,
    ) -> Result<Vec<u8>, BusError> {
        self.calls.set(self.calls.get() + 1);
        if self.stalled.get() {
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
        handler(iface, method, body)
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
