//! webd's service: `os.lazy.webd.light.v1` over a [`Fabric`] and an [`Engine`].
//!
//! The hosting program receives a request, wraps it as a [`Call`] with the sender's kernel-stamped
//! [`Origin`], and hands it to [`Service::handle`], which returns the reply. Everything the engine
//! says is queued by [`EventQueue`] on WebKit's thread; [`Service::pump`] (on the service thread)
//! turns it into events on the clients' endpoints and sends the liveness `Ping`s. Nothing here
//! blocks: a slow client loses frames, never holds the engine or another client up.
//!
//! * `service/calls.rs`: one function per client call.
//! * `service/events.rs`: the engine's news, delivery to clients, client death.

mod calls;
mod events;

use std::collections::VecDeque;
use std::sync::Arc;

use libmessenger::{Object, Parcel};
use messenger_generated::os_lazy_webd_light_v1 as wire;

use crate::config::Refusal;
use crate::engine::Engine;
use crate::events::EventQueue;
use crate::fabric::{errno, Fabric};
use crate::ident::{ClientKey, Groups, Origin};
use crate::limits::QUEUE_KEPT;
use crate::reply;
use crate::view::Views;

/// One request as the host received it.
pub struct Call {
    /// Who sent it, as the kernel stamped it.
    pub origin: Origin,
    /// The request: header (interface and method) and TLV body.
    pub parcel: Parcel,
    /// The objects the kernel installed for the request, in object-list order. The service owns
    /// them: each is kept, mapped, read or closed, exactly once.
    pub objects: Vec<Object>,
}

/// Something the host may want to trace (the crate cannot call `xui_app::trace`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    ViewCreated {
        view: u64,
        client: ClientKey,
        width: u32,
        height: u32,
        group: i32,
    },
    /// `why`: `"destroyed"` (the client asked), `"client gone"` or `"shutdown"`.
    ViewEnded { view: u64, why: &'static str },
    /// The engine gave the view up (`LostReason` value).
    ViewLost { view: u64, reason: u32 },
    /// A call was refused; `method` is the method id.
    Refused {
        method: u32,
        code: i64,
        text: &'static str,
    },
    /// A frame went nowhere: no free slot, a stale size, or a full client queue.
    FrameDropped { view: u64, why: &'static str },
    /// A marker line from the engine (`webd_ui_say`).
    Engine(String),
}

/// The service.
pub struct Service<E: Engine, F: Fabric> {
    engine: E,
    fabric: F,
    queue: Arc<EventQueue>,
    /// The login session webd serves; a call from another session is refused (`EACCES`).
    session: u64,
    views: Views,
    groups: Groups,
    notes: VecDeque<Note>,
    /// When the next `Ping` is due (milliseconds on the host's clock); `None` until the first `pump`.
    next_ping_ms: Option<u64>,
    /// The host's clock at the last `pump` (0 before the first), for the per-client back-off.
    now_ms: u64,
}

impl<E: Engine, F: Fabric> Service<E, F> {
    /// A service for `session` over `engine` and `fabric`; `queue` is the sink the engine's
    /// callbacks were given.
    pub fn new(engine: E, fabric: F, queue: Arc<EventQueue>, session: u64) -> Self {
        Service {
            engine,
            fabric,
            queue,
            session,
            views: Views::default(),
            groups: Groups::default(),
            notes: VecDeque::new(),
            next_ping_ms: None,
            now_ms: 0,
        }
    }

    /// Answer one request. The reply is for the sender when it waits for one (a oneway method's
    /// is dropped by the host).
    pub fn handle(&mut self, call: Call) -> Parcel {
        let Call {
            origin,
            parcel,
            objects,
        } = call;
        let method = parcel.header.method;
        let mut pending = objects;
        let result = if parcel.header.interface_id != wire::INTERFACE_ID {
            Err(Refusal::new(errno::ENOTSUP, "not os.lazy.webd.light.v1"))
        } else if origin.session != self.session {
            Err(Refusal::new(
                errno::EACCES,
                "caller is in another login session",
            ))
        } else {
            self.dispatch(origin.client(), method, &parcel.body, &mut pending)
        };
        // Whatever the handler did not keep, map or read is closed here: a refused request must
        // leave nothing in the service's handle table.
        for object in pending {
            self.discard(object);
        }
        match result {
            Ok(body) => reply::ok(method, body),
            Err(refusal) => {
                if !is_oneway(method) {
                    self.note(Note::Refused {
                        method,
                        code: refusal.code,
                        text: refusal.text,
                    });
                }
                reply::error(method, refusal)
            }
        }
    }

    fn dispatch(
        &mut self,
        client: ClientKey,
        method: u32,
        body: &[u8],
        pending: &mut Vec<Object>,
    ) -> Result<Vec<u8>, Refusal> {
        match method {
            wire::METHOD_CREATEVIEW => self.create_view(client, body, pending),
            wire::METHOD_DESTROYVIEW => self.destroy_view(client, body),
            wire::METHOD_ATTACHSLOT => self.attach_slot(client, body, pending),
            wire::METHOD_RELEASESLOT => self.release_slot(client, body),
            wire::METHOD_RESIZE => self.resize(client, body),
            wire::METHOD_SETVISIBLE => self.set_visible(client, body),
            wire::METHOD_REFRESH => self.refresh(client, body),
            wire::METHOD_LOADHTML => self.load_html(client, body, pending),
            wire::METHOD_MOUSE => self.mouse(client, body),
            wire::METHOD_WHEEL => self.wheel(client, body),
            wire::METHOD_KEY => self.key(client, body),
            wire::METHOD_FOCUS => self.focus(client, body),
            _ => Err(Refusal::new(errno::ENOTSUP, "unknown method")),
        }
    }

    /// Close an object nobody claimed.
    fn discard(&self, object: Object) {
        match object {
            Object::Channel(handle) => self.fabric.close_channel(handle),
            Object::Buffer(handle) => self.fabric.close_buffer(handle),
        }
    }

    fn note(&mut self, note: Note) {
        if self.notes.len() >= QUEUE_KEPT {
            self.notes.pop_front();
        }
        self.notes.push_back(note);
    }

    /// Take the notes since the last call, oldest first.
    pub fn take_notes(&mut self) -> Vec<Note> {
        self.notes.drain(..).collect()
    }

    /// The engine, for the host (e.g. to ask for its children).
    pub fn engine(&self) -> &E {
        &self.engine
    }

    pub fn fabric(&self) -> &F {
        &self.fabric
    }

    /// Live and lost views in the table.
    pub fn view_count(&self) -> usize {
        self.views.len()
    }

    /// Views `client` holds, lost ones included.
    pub fn client_views(&self, client: ClientKey) -> usize {
        self.views.count_of(client)
    }

    /// The group the service gave `client`, if it has one (for tests and traces).
    pub fn group_of(&self, client: ClientKey) -> Option<i32> {
        self.groups.get(client)
    }
}

/// Methods that have no reply; a refusal of one is not worth a note (input floods).
fn is_oneway(method: u32) -> bool {
    matches!(
        method,
        wire::METHOD_RELEASESLOT
            | wire::METHOD_SETVISIBLE
            | wire::METHOD_REFRESH
            | wire::METHOD_MOUSE
            | wire::METHOD_WHEEL
            | wire::METHOD_KEY
            | wire::METHOD_FOCUS
    )
}

/// Remove the first pending object that is this channel or buffer: the handler has taken it over.
fn claim(pending: &mut Vec<Object>, object: Object) {
    if let Some(index) = pending.iter().position(|other| *other == object) {
        pending.remove(index);
    }
}
