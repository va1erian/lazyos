//! An in-memory Messenger fabric for this crate's tests.
//!
//! `rhai-lazy`'s own mock is private to its tests, so this one is built here
//! on its public `Bus` seam. Services are closures over decoded Rhai values:
//! the request is decoded with the schema codec and the reply encoded with it,
//! so a codec error in a generated snippet fails the call exactly as it would
//! on the real fabric. The topic broker lives in `broker.rs`.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use rhai::{Dynamic, Map};
use rhai_lazy::msg::schema::{self, Interface, Method};
use rhai_lazy::msg::{codec, Bus, BusError, Incoming, Wait};

use super::broker::Broker;

/// A decoded request.
pub struct Req {
    pub method: &'static Method,
    pub args: Map,
}

/// A service's answer: the reply fields by name, or an errno and its text.
pub type Reply = Result<Map, (u32, String)>;

type Handler = Rc<dyn Fn(&FakeBus, &Req) -> Reply>;

/// Something the harness does when the script under test goes idle.
pub type Action = Box<dyn FnOnce(&FakeBus)>;

/// Who owns a registered name, as the registry reports it.
#[derive(Clone, Copy, Default)]
pub struct Owner {
    pub uid: u64,
    pub label: u64,
    pub session: u64,
}

struct Service {
    name: String,
    endpoint: u64,
    interfaces: Vec<u64>,
    owner: Owner,
    slot: u64,
    handler: Option<Handler>,
}

/// The fabric. Interior mutability: `Bus` takes `&self`.
pub struct FakeBus {
    services: RefCell<Vec<Service>>,
    next: Cell<u64>,
    pub broker: RefCell<Broker>,
    /// Requests waiting on endpoints the script serves.
    pub inbox: RefCell<VecDeque<(u64, Incoming)>>,
    /// Replies the script sent, by transaction.
    pub replies: RefCell<Vec<(u64, Vec<u8>)>>,
    scenario: RefCell<VecDeque<Action>>,
    pub clock: Cell<u64>,
    idle_left: Cell<u32>,
    pub calls: Cell<usize>,
    next_txn: Cell<u64>,
}

/// The errno the fabric answers with once the scenario is spent, which ends
/// a `msg::run()` that would never return.
pub const IDLE_ERRNO: i64 = -125;

impl FakeBus {
    pub fn new() -> Rc<Self> {
        let bus = Rc::new(FakeBus {
            services: RefCell::new(Vec::new()),
            next: Cell::new(100),
            broker: RefCell::new(Broker::default()),
            inbox: RefCell::new(VecDeque::new()),
            replies: RefCell::new(Vec::new()),
            scenario: RefCell::new(VecDeque::new()),
            clock: Cell::new(1_000),
            idle_left: Cell::new(3),
            calls: Cell::new(0),
            next_txn: Cell::new(1),
        });
        super::broker::install(&bus);
        bus
    }

    /// Register `name` for `interface` (by name) with a handler.
    pub fn serve(
        &self,
        name: &str,
        interface: &str,
        owner: Owner,
        handler: impl Fn(&FakeBus, &Req) -> Reply + 'static,
    ) -> u64 {
        let id = schema::interface(interface)
            .unwrap_or_else(|| panic!("no interface {interface}"))
            .id;
        self.add(name, vec![id], owner, Some(Rc::new(handler)))
    }

    fn add(&self, name: &str, interfaces: Vec<u64>, owner: Owner, handler: Option<Handler>) -> u64 {
        let endpoint = self.next.get();
        self.next.set(endpoint + 1);
        let slot = 10 + self.services.borrow().len() as u64;
        self.services.borrow_mut().push(Service {
            name: name.to_string(),
            endpoint,
            interfaces,
            owner,
            slot,
            handler,
        });
        endpoint
    }

    /// A name some task owns that no handler answers for (an app's own).
    pub fn serve_external(&self, name: &str, interfaces: &[u64], owner: Owner) -> u64 {
        self.add(name, interfaces.to_vec(), owner, None)
    }

    /// Register `interface` under its default service name; every method
    /// answers with all reply fields at their zero value.
    pub fn serve_zero(&self, interface: &Interface) {
        self.serve(
            interface.default_service(),
            interface.name,
            Owner::default(),
            |_, _| Ok(Map::new()),
        );
    }

    /// The registry rows for the names registered now.
    pub fn registry_rows(&self) -> Vec<crate::live::RegistryRow> {
        self.services
            .borrow()
            .iter()
            .map(|s| crate::live::RegistryRow {
                name: s.name.clone(),
                object: s.endpoint,
                owner: s.slot,
                interfaces: s.interfaces.clone(),
                lease_remaining: 0,
                owner_uid: s.owner.uid,
                owner_label: s.owner.label,
                owner_session: s.owner.session,
            })
            .collect()
    }

    /// Queue something to happen the next time the script waits and nothing
    /// is ready.
    pub fn on_idle(&self, action: impl FnOnce(&FakeBus) + 'static) {
        self.scenario.borrow_mut().push_back(Box::new(action));
    }

    /// How many idle waits are allowed once the scenario is spent.
    pub fn set_idle_budget(&self, ticks: u32) {
        self.idle_left.set(ticks);
    }

    /// A waiting script found nothing. Runs the next scenario step and says
    /// whether the script may keep waiting.
    pub fn idle(&self) -> bool {
        let next = self.scenario.borrow_mut().pop_front();
        if let Some(action) = next {
            action(self);
            return true;
        }
        let left = self.idle_left.get();
        if left == 0 {
            return false;
        }
        self.idle_left.set(left - 1);
        true
    }

    /// Queue a call to the name a script serves; the reply lands in
    /// `replies` under the returned transaction.
    pub fn inject_request(&self, name: &str, method: &str, args: Map) -> u64 {
        let (endpoint, iface_id) = {
            let services = self.services.borrow();
            let service = services
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{name} is not served"));
            (service.endpoint, service.interfaces[0])
        };
        let iface = schema::interfaces()
            .iter()
            .find(|i| i.id == iface_id)
            .expect("served interface");
        let method = iface.method(method).expect("method");
        let body = codec::encode_named(iface, method.params, &args, 0).expect("encode request");
        let txn = self.next_txn.get();
        self.next_txn.set(txn + 1);
        self.inbox.borrow_mut().push_back((
            endpoint,
            Incoming {
                interface: iface.id,
                method: method.id,
                txn: (!method.oneway).then_some(txn),
                body,
            },
        ));
        txn
    }

    /// The reply the script gave transaction `txn`, decoded.
    pub fn reply_to(&self, name: &str, method: &str, txn: u64) -> Result<Map, (u32, String)> {
        let iface = {
            let services = self.services.borrow();
            let id = services
                .iter()
                .find(|s| s.name == name)
                .expect("service")
                .interfaces[0];
            schema::interfaces()
                .iter()
                .find(|i| i.id == id)
                .expect("iface")
        };
        let method = iface.method(method).expect("method");
        let body = self
            .replies
            .borrow()
            .iter()
            .find(|(t, _)| *t == txn)
            .map(|(_, b)| b.clone())
            .expect("no reply for the transaction");
        if let Some((code, message)) = codec::reply_error(&body) {
            return Err((code, message));
        }
        Ok(codec::decode_named(iface, method.returns, &body, 0).expect("decode reply"))
    }

    fn handler_of(&self, endpoint: u64) -> Option<Handler> {
        self.services
            .borrow()
            .iter()
            .find(|s| s.endpoint == endpoint)
            .and_then(|s| s.handler.clone())
    }
}

fn parcel_error() -> BusError {
    BusError::errno(-22)
}

impl Bus for FakeBus {
    fn resolve(&self, name: &str) -> Result<u64, BusError> {
        self.services
            .borrow()
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.endpoint)
            .ok_or_else(|| BusError::errno(-2))
    }

    fn call(
        &self,
        endpoint: u64,
        interface: u64,
        method: u32,
        body: &[u8],
        wait: Wait,
    ) -> Result<Vec<u8>, BusError> {
        self.calls.set(self.calls.get() + 1);
        let handler = self
            .handler_of(endpoint)
            .ok_or_else(|| BusError::errno(-32))?;
        let iface = schema::interfaces()
            .iter()
            .find(|i| i.id == interface)
            .ok_or_else(parcel_error)?;
        let def = iface.method_by_id(method).ok_or_else(parcel_error)?;
        // A request the schema codec cannot decode is what a codec error in a
        // snippet would be on the real fabric.
        let args = codec::decode_named(iface, def.params, body, 0).map_err(|e| BusError {
            code: -22,
            message: format!("bad request: {e}"),
        })?;
        let request = Req { method: def, args };
        match handler(self, &request) {
            Ok(reply) => {
                let out =
                    codec::encode_named(iface, def.returns, &reply, 0).map_err(|e| BusError {
                        code: -5,
                        message: format!("service reply: {e}"),
                    })?;
                Ok(out)
            }
            Err((110, _)) => {
                self.clock.set(self.clock.get() + wait.millis());
                Err(BusError::errno(-110))
            }
            Err((125, _)) => Err(idle_error()),
            Err((code, message)) => Ok(error_body(code, &message)),
        }
    }

    fn send(
        &self,
        endpoint: u64,
        interface: u64,
        method: u32,
        body: &[u8],
    ) -> Result<(), BusError> {
        // A one-way message to a mock service runs its handler, replyless.
        if self.handler_of(endpoint).is_some() {
            let _ = self.call(endpoint, interface, method, body, Wait::Poll);
        }
        Ok(())
    }

    fn names(&self) -> Result<Vec<String>, BusError> {
        Ok(self
            .services
            .borrow()
            .iter()
            .map(|s| s.name.clone())
            .collect())
    }

    fn register(
        &self,
        name: &str,
        interfaces: &[u64],
        interface_names: &[&str],
    ) -> Result<u64, BusError> {
        assert_eq!(interfaces.len(), interface_names.len());
        if self.services.borrow().iter().any(|s| s.name == name) {
            return Err(BusError::errno(-17));
        }
        Ok(self.add(name, interfaces.to_vec(), Owner::default(), None))
    }

    fn recv(&self, endpoint: u64, wait: Wait) -> Result<Option<Incoming>, BusError> {
        let ready = {
            let mut inbox = self.inbox.borrow_mut();
            inbox
                .iter()
                .position(|(ep, _)| *ep == endpoint)
                .and_then(|i| inbox.remove(i))
        };
        if let Some((_, request)) = ready {
            return Ok(Some(request));
        }
        self.clock.set(self.clock.get() + wait.millis().max(1));
        if self.idle() {
            Ok(None)
        } else {
            Err(idle_error())
        }
    }

    fn reply(&self, txn: u64, _iface: u64, _method: u32, body: &[u8]) -> Result<(), BusError> {
        self.replies.borrow_mut().push((txn, body.to_vec()));
        Ok(())
    }

    fn unregister(&self, name: &str, _endpoint: u64) -> Result<(), BusError> {
        self.services.borrow_mut().retain(|s| s.name != name);
        Ok(())
    }

    fn clock_ms(&self) -> u64 {
        self.clock.get()
    }
}

/// The error that ends a `msg::run()` the scenario has nothing more for.
pub fn idle_error() -> BusError {
    BusError {
        code: IDLE_ERRNO,
        message: "harness: the script is idle and the scenario is spent".to_string(),
    }
}

/// A structured service error body (the standard error field).
pub fn error_body(code: u32, message: &str) -> Vec<u8> {
    let mut enc = libmessenger::Encoder::new();
    enc.error(messenger_generated::errors::ERROR_FIELD, code, message)
        .expect("error field");
    enc.finish()
}

/// A Rhai value for a plain string, for building arguments in tests.
pub fn s(text: &str) -> Dynamic {
    Dynamic::from(text.to_string())
}
