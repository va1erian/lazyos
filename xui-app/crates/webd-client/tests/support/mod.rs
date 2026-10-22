//! The other end of the client in the tests: the service half (`xui-webd`) over its in-memory
//! fabric and recording engine, behind a [`Transport`]. The tests play the engine by calling the
//! queue the service drains; `recv` runs one turn of the service's housekeeping first, as its
//! thread would between two of the client's polls.

#![allow(dead_code)]

pub mod full;
pub mod web;
pub mod web_pages;

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use libmessenger::{Decoder, Header, Kind, Object, Parcel, VERSION};
use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_webd_light_v1 as wire;
use xui_webd::engine::{Engine, EngineEvents};
use xui_webd::events::EventQueue;
use xui_webd::fullconfig::Places;
use xui_webd::ident::Origin;
use xui_webd::mem::{MemFabric, Shared};
use xui_webd::recording::RecordingEngine;
use xui_webd::{Call, Service};
use xui_webd_client::transport::{RawEvent, ScopedTransport, SharedMem, Transport};
use xui_webd_client::{LightConfig, LightWebView};

pub const SESSION: u64 = 1;

/// The service and everything a test wants to reach on its side; `E` is the engine the tests play
/// (the recording double, or a fake web that answers loads).
pub struct World<E: Engine = RecordingEngine> {
    pub service: Service<E, MemFabric>,
    pub engine: E,
    pub fabric: MemFabric,
    pub queue: Arc<EventQueue>,
    /// The service is not running: calls fail with `-ENOENT`, sends with `-EPIPE`.
    pub down: bool,
    /// Make `recv` fail with this errno (the service's end of the events closed).
    pub fail_recv: Option<i64>,
    /// Events waiting for the client, per channel.
    pending: BTreeMap<u64, VecDeque<RawEvent>>,
    /// The service's clock in milliseconds.
    pub clock: u64,
    /// Every oneway message the client sent: `(method, body)`.
    pub sent: Vec<(u32, Vec<u8>)>,
    /// How many channel handles the client closed.
    pub channels_closed: usize,
    /// The next this-many oneway sends fail with `-EAGAIN` (webd's inbox is full) and are lost.
    pub eagain: usize,
}

pub type SharedWorld<E = RecordingEngine> = Rc<RefCell<World<E>>>;

pub fn world() -> SharedWorld {
    world_with(
        RecordingEngine::new(),
        Arc::new(EventQueue::new()),
        Places::default(),
    )
}

/// A world over `engine`, whose callbacks fill `queue`, with webd's profile and Downloads folders
/// at `places` (a `browser` view needs them).
pub fn world_with<E: Engine + Clone>(
    engine: E,
    queue: Arc<EventQueue>,
    places: Places,
) -> SharedWorld<E> {
    let fabric = MemFabric::new();
    let service =
        Service::new(engine.clone(), fabric.clone(), queue.clone(), SESSION).with_places(places);
    Rc::new(RefCell::new(World {
        service,
        engine,
        fabric,
        queue,
        down: false,
        fail_recv: None,
        pending: BTreeMap::new(),
        clock: 0,
        sent: Vec::new(),
        channels_closed: 0,
        eagain: 0,
    }))
}

impl<E: Engine> World<E> {
    /// The engine paints a whole frame of one colour (RGBA).
    pub fn paint(&self, view: u64, w: i32, h: i32, rgba: [u8; 4]) {
        let pixels: Vec<u8> = rgba
            .iter()
            .copied()
            .cycle()
            .take((w * h * 4) as usize)
            .collect();
        self.queue.frame(view, &pixels, w, h);
    }

    /// Deliver the engine's news to the clients now.
    pub fn pump(&mut self) {
        self.clock += 1;
        self.service.pump(self.clock);
    }

    /// Put an event on `channel` as if the service had sent it.
    pub fn inject(&mut self, channel: u64, method: u32, body: Vec<u8>) {
        self.pending
            .entry(channel)
            .or_default()
            .push_back((method, body));
    }
}

/// A client's transport: the world, as seen by the app labelled `label`, over one interface (the
/// light one, or the full one for a `FullWebView`).
pub struct TestTransport<E: Engine = RecordingEngine> {
    pub world: SharedWorld<E>,
    pub origin: Origin,
    pub interface: u64,
}

impl<E: Engine> Clone for TestTransport<E> {
    fn clone(&self) -> Self {
        TestTransport {
            world: self.world.clone(),
            origin: self.origin,
            interface: self.interface,
        }
    }
}

pub fn origin(label: u32) -> Origin {
    Origin {
        uid: 1000,
        label,
        session: SESSION,
        sender: 100 + u64::from(label),
    }
}

pub struct Mem {
    handle: u64,
    shared: Shared,
}

impl SharedMem for Mem {
    fn handle(&self) -> u64 {
        self.handle
    }

    fn len(&self) -> usize {
        self.shared.borrow().len()
    }

    fn write(&mut self, bytes: &[u8]) {
        self.shared.borrow_mut()[..bytes.len()].copy_from_slice(bytes);
    }

    fn read(&self, into: &mut [u8]) {
        into.copy_from_slice(&self.shared.borrow()[..into.len()]);
    }

    fn read_at(&self, offset: usize, into: &mut [u8]) {
        into.copy_from_slice(&self.shared.borrow()[offset..offset + into.len()]);
    }
}

impl<E: Engine> TestTransport<E> {
    pub fn new(world: &SharedWorld<E>, label: u32) -> Self {
        TestTransport {
            world: world.clone(),
            origin: origin(label),
            interface: wire::INTERFACE_ID,
        }
    }

    /// The transport of a full view: `os.lazy.webd.view.v1`.
    pub fn full(world: &SharedWorld<E>, label: u32) -> Self {
        TestTransport {
            interface: messenger_generated::os_lazy_webd_view_v1::INTERFACE_ID,
            ..Self::new(world, label)
        }
    }

    fn call_parcel(
        &self,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        objects: Vec<Object>,
    ) -> Call {
        Call {
            origin: self.origin,
            parcel: Parcel {
                header: Header {
                    version: VERSION,
                    flags: 0,
                    interface_id: interface,
                    method,
                    txn_id: 0,
                    reply_to: 0,
                    deadline_ns: 0,
                },
                body,
                objects: Vec::new(),
            },
            objects,
        }
    }

    fn call_as(
        &mut self,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        objects: Vec<Object>,
    ) -> Result<Vec<u8>, i64> {
        let mut world = self.world.borrow_mut();
        if world.down {
            return Err(-2);
        }
        let reply = world
            .service
            .handle(self.call_parcel(interface, method, body, objects));
        let mut decoder = Decoder::new(&reply.body);
        if let Ok(Some(field)) = decoder.next() {
            if field.kind == Kind::Error && field.id == ERROR_FIELD {
                return Err(-i64::from(field.error_parts().unwrap().0));
            }
        }
        Ok(reply.body)
    }
}

impl<E: Engine> ScopedTransport for TestTransport<E> {
    fn call_through(
        &mut self,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        objects: Vec<Object>,
    ) -> Result<Vec<u8>, i64> {
        self.call_as(interface, method, body, objects)
    }
}

impl<E: Engine> Transport for TestTransport<E> {
    type Mem = Mem;

    fn call(&mut self, method: u32, body: Vec<u8>, objects: Vec<Object>) -> Result<Vec<u8>, i64> {
        let interface = self.interface;
        self.call_as(interface, method, body, objects)
    }

    fn send(&mut self, method: u32, body: Vec<u8>) -> Result<(), i64> {
        let mut world = self.world.borrow_mut();
        if world.down {
            return Err(-32);
        }
        if world.eagain > 0 {
            world.eagain -= 1;
            return Err(-11);
        }
        world.sent.push((method, body.clone()));
        let call = self.call_parcel(self.interface, method, body, Vec::new());
        world.service.handle(call);
        Ok(())
    }

    fn create_pair(&mut self) -> Result<(u64, u64), i64> {
        let channel = self.world.borrow().fabric.new_channel();
        Ok((channel, channel))
    }

    fn recv(&mut self, channel: u64) -> Result<Option<RawEvent>, i64> {
        let mut world = self.world.borrow_mut();
        if let Some(code) = world.fail_recv {
            return Err(code);
        }
        world.pump();
        // An event of another interface is read and dropped, as the real transport does (method 0
        // is no event: the client counts it as ignored).
        let interface = self.interface;
        let fresh = world
            .fabric
            .take_tagged(channel)
            .into_iter()
            .map(|(tag, method, body)| match tag == interface {
                true => (method, body),
                false => (0, Vec::new()),
            });
        let queue = world.pending.entry(channel).or_default();
        queue.extend(fresh);
        Ok(queue.pop_front())
    }

    fn alloc(&mut self, bytes: usize) -> Result<Mem, i64> {
        let (handle, shared) = self.world.borrow().fabric.new_buffer(bytes);
        Ok(Mem { handle, shared })
    }

    fn close_channel(&mut self, _handle: u64) {
        // The in-memory fabric's channel is one number for both ends, which the service closes.
        self.world.borrow_mut().channels_closed += 1;
    }
}

/// A view of the app labelled `label` on a fresh world, not created yet.
pub fn view(label: u32) -> (SharedWorld, LightWebView<TestTransport>) {
    let world = world();
    let view = LightWebView::new(TestTransport::new(&world, label), LightConfig::default());
    (world, view)
}
