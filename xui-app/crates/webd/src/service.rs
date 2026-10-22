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
mod create;
mod deliver;
mod events;
mod full;
mod notify;
mod notify_client;

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use lazydownloads::Saver;
use libmessenger::{Object, Parcel};
use messenger_generated::os_lazy_webd_browser_v1 as browser_wire;
use messenger_generated::os_lazy_webd_light_v1 as wire;
use messenger_generated::os_lazy_webd_net_v1 as net_wire;
use messenger_generated::os_lazy_webd_view_v1 as view_wire;

use crate::config::Refusal;
use crate::engine::Engine;
use crate::events::EventQueue;
use crate::fabric::{errno, Fabric};
use crate::fullconfig::{Places, Profile, Scope};
use crate::ident::{ClientKey, Groups, Origin};
use crate::limits::QUEUE_KEPT;
use crate::outside::{Nowhere, Outside};
use crate::reply;
use crate::view::Views;

/// Which interface a request came through: the light one, or the full one (`CreateView` also
/// through a scope, which is what the view may reach).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Api {
    Light,
    Full(Scope),
}

impl Api {
    fn of(interface: u64) -> Option<Api> {
        match interface {
            wire::INTERFACE_ID => Some(Api::Light),
            view_wire::INTERFACE_ID => Some(Api::Full(Scope::View)),
            net_wire::INTERFACE_ID => Some(Api::Full(Scope::Net)),
            browser_wire::INTERFACE_ID => Some(Api::Full(Scope::Browser)),
            _ => None,
        }
    }

    /// Calls on a view: the light interface for a light view, the full one for a full view.
    pub fn full(self) -> bool {
        matches!(self, Api::Full(_))
    }
}

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
    /// Something a full view's page did that the service decided about (`what`: `held`,
    /// `held refused`, `hand-off`, `hand-off refused`, `dialog dismissed`, `toast`, ...).
    Full { view: u64, what: &'static str },
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
    /// The system around webd (hand-offs, toasts), for full views.
    outside: Box<dyn Outside>,
    /// Where the browser profile and downloads live; a service without them makes no browser view.
    places: Places,
    /// The browser profile's downloads (made at its first view that may download).
    saver: Option<Arc<Saver>>,
    /// Which sites may show notifications, per profile, while webd runs (`NotifyPermission`).
    permissions: BTreeMap<Profile, BTreeMap<String, bool>>,
    /// What each client has on screen and its pace, so one client's pages cannot use up the shell's
    /// allowance for webd (`service/notify.rs`).
    client_toasts: BTreeMap<ClientKey, notify::ClientToasts>,
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
            outside: Box::new(Nowhere),
            places: Places::default(),
            saver: None,
            permissions: BTreeMap::new(),
            client_toasts: BTreeMap::new(),
        }
    }

    /// Hand addresses and toasts to `outside` (the host's Messenger clients).
    pub fn with_outside(mut self, outside: Box<dyn Outside>) -> Self {
        self.outside = outside;
        self
    }

    /// Keep the browser profile and the downloads at `places`.
    pub fn with_places(mut self, places: Places) -> Self {
        self.places = places;
        self
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
        let interface = parcel.header.interface_id;
        let mut pending = objects;
        let result = match Api::of(interface) {
            None => Err(Refusal::new(errno::ENOTSUP, "not an interface of webd")),
            Some(_) if origin.session != self.session => Err(Refusal::new(
                errno::EACCES,
                "caller is in another login session",
            )),
            Some(api) => self.dispatch(origin.client(), api, method, &parcel.body, &mut pending),
        };
        // Whatever the handler did not keep, map or read is closed here: a refused request must
        // leave nothing in the service's handle table.
        for object in pending {
            self.discard(object);
        }
        match result {
            Ok(body) => reply::ok(interface, method, body),
            Err(refusal) => {
                if !is_oneway(method) {
                    self.note(Note::Refused {
                        method,
                        code: refusal.code,
                        text: refusal.text,
                    });
                }
                reply::error(interface, method, refusal)
            }
        }
    }

    /// The shared methods have one id and wire shape on both interfaces (`tools/webd/test_idl.py`), so
    /// they are decoded with the light codec whichever one they came through; `api` says which kind
    /// of view they may touch. A scope interface has `CreateView` only.
    fn dispatch(
        &mut self,
        client: ClientKey,
        api: Api,
        method: u32,
        body: &[u8],
        pending: &mut Vec<Object>,
    ) -> Result<Vec<u8>, Refusal> {
        let scope_only = matches!(api, Api::Full(Scope::Net | Scope::Browser));
        if scope_only && method != wire::METHOD_CREATEVIEW {
            return Err(Refusal::new(
                errno::ENOTSUP,
                "a scope interface only makes views",
            ));
        }
        let full = api.full();
        match (api, method) {
            (Api::Light, wire::METHOD_CREATEVIEW) => self.create_view(client, body, pending),
            (Api::Full(scope), wire::METHOD_CREATEVIEW) => {
                self.create_full_view(client, scope, body, pending)
            }
            (_, wire::METHOD_DESTROYVIEW) => self.destroy_view(client, full, body),
            (_, wire::METHOD_ATTACHSLOT) => self.attach_slot(client, full, body, pending),
            (_, wire::METHOD_RELEASESLOT) => self.release_slot(client, full, body),
            (_, wire::METHOD_RESIZE) => self.resize(client, full, body),
            (_, wire::METHOD_SETVISIBLE) => self.set_visible(client, full, body),
            (_, wire::METHOD_REFRESH) => self.refresh(client, full, body),
            (_, wire::METHOD_LOADHTML) => self.load_html(client, full, body, pending),
            (_, wire::METHOD_MOUSE) => self.mouse(client, full, body),
            (_, wire::METHOD_WHEEL) => self.wheel(client, full, body),
            (_, wire::METHOD_KEY) => self.key(client, full, body),
            (_, wire::METHOD_FOCUS) => self.focus(client, full, body),
            (Api::Full(_), _) => self.full_call(client, method, body, pending),
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
