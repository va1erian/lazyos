//! The full view (`os.lazy.webd.view.v1`, idl/webd.midl, docs/webd-plan.md 3.3 to 3.5): what a
//! browser needs of webd. It is made through a [`Scope`] (the interface its `CreateView` goes
//! through, which the kernel checks against the app's manifest), loads addresses, goes back and
//! forward, answers the page's questions (dialogs, held navigations, notification permissions),
//! downloads, blocks content, and keeps a persistent [`Picture`] from whole frames and deltas.
//!
//! The slots follow the light view's rule: **a slot is webd's until the client has copied out of
//! it**. [`FullWebView::poll`] applies each frame as it reads it (a delta only means something on
//! top of the frame before it, so none is skipped) and releases its slot at once. A delta that has
//! nothing to apply to (the client missed the frame before it) is refused and a whole frame asked
//! for. Band slots (3 and 4, with `deltas`) carry rows outside the view, copied into [`Band`]s.
//!
//! * `calls.rs`: navigation, the answers, downloads and content blocking.
//! * `frames.rs`: `poll`, the frames, bands and the other events.
//! * `input.rs`: oneway input and the messages that must arrive (`oneway.rs`).

mod calls;
pub mod event;
mod frames;
mod input;
mod oneway;

use libmessenger::Buffer;
use messenger_generated::os_lazy_webd_browser_v1 as browser_wire;
use messenger_generated::os_lazy_webd_net_v1 as net_wire;
use messenger_generated::os_lazy_webd_view_v1 as wire;

use crate::event::{Error, Lost};
use crate::picture::Picture;
use crate::transport::{errno, ScopedTransport, SharedMem};
use oneway::Owed;

pub use event::{blocking_status_name, Band, FrameInfo, FullEvent, FullState, Rect, Shift};
pub use wire::ViewConfig;

/// Frame slots a view attaches (0 to 2).
pub const FRAME_SLOTS: usize = 3;
/// Band slots a view with deltas attaches (3 and 4).
pub const BAND_SLOTS: usize = 2;
/// Every slot number a view may use.
pub const ALL_SLOTS: usize = FRAME_SLOTS + BAND_SLOTS;
/// Rows a band slot holds (`width * BAND_ROWS * 4` bytes).
pub const BAND_ROWS: u32 = 1024;

/// What a view may reach: the interface its `CreateView` goes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// `os.lazy.webd.view.v1`: no network (`DataOnly`).
    View,
    /// `os.lazy.webd.net.v1`: `http:` and `https:`.
    Net,
    /// `os.lazy.webd.browser.v1`: the `browser` profile, downloads, blocking, hand-offs.
    Browser,
}

impl Scope {
    /// The interface id `CreateView` is sent with.
    pub fn interface(self) -> u64 {
        match self {
            Scope::View => wire::INTERFACE_ID,
            Scope::Net => net_wire::INTERFACE_ID,
            Scope::Browser => browser_wire::INTERFACE_ID,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Scope::View => "view",
            Scope::Net => "net",
            Scope::Browser => "browser",
        }
    }
}

/// What the client has done, for traces and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FullCounters {
    /// Whole frames copied into the picture.
    pub whole: u64,
    /// Deltas applied to it.
    pub deltas: u64,
    pub bands: u64,
    /// Frames refused (a stale size, a delta with no base) and events that did not decode.
    pub ignored: u64,
    /// Whole frames asked for because a delta could not be applied.
    pub refreshes: u64,
}

struct Live {
    view: u64,
    events: u64,
}

/// One full view of `webd`, over a [`ScopedTransport`] bound to `os.lazy.webd.view.v1`.
pub struct FullWebView<T: ScopedTransport> {
    transport: T,
    scope: Scope,
    config: ViewConfig,
    size: (u32, u32),
    live: Option<Live>,
    lost: Option<Lost>,
    /// By slot number; `None` for one not attached.
    slots: Vec<Option<T::Mem>>,
    picture: Picture,
    /// The picture is the last frame webd sent, so a delta may be applied to it.
    base: bool,
    last_seq: u64,
    page: FullState,
    scroll: (i32, i32, bool),
    visible: bool,
    focused: bool,
    owed: Owed,
    counters: FullCounters,
}

impl<T: ScopedTransport> FullWebView<T> {
    /// A view that is not created yet: [`FullWebView::create`] asks for one through `scope` with
    /// `config`.
    pub fn new(transport: T, scope: Scope, config: ViewConfig) -> Self {
        FullWebView {
            transport,
            scope,
            config,
            size: (0, 0),
            live: None,
            lost: None,
            slots: Vec::new(),
            picture: Picture::default(),
            base: false,
            last_seq: 0,
            page: FullState::default(),
            scroll: (0, 0, false),
            visible: true,
            focused: false,
            owed: Owed::default(),
            counters: FullCounters::default(),
        }
    }

    /// Ask webd for a view of `width` x `height` and attach its slots (3 frame slots, and 2 band
    /// slots with `deltas`). A view that exists is destroyed first. [`Error::NotRunning`] when webd
    /// is not there, [`Error::Refused`] with `EACCES` when the scope does not allow the config.
    pub fn create(&mut self, width: u32, height: u32) -> Result<(), Error> {
        self.teardown();
        let (kept, sent) = self
            .transport
            .create_pair()
            .map_err(|code| Error::Refused(-code))?;
        let args = wire::CreateViewArgs {
            config: self.config.clone(),
            width,
            height,
            events: sent,
        };
        let Ok((body, objects)) = wire::encode_create_view_args(&args) else {
            self.transport.close_channel(kept);
            self.transport.close_channel(sent);
            return Err(Error::Refused(errno::EINVAL));
        };
        let interface = self.scope.interface();
        let reply = self
            .transport
            .call_through(interface, wire::METHOD_CREATEVIEW, body, objects);
        let view = match reply {
            Ok(body) => wire::decode_create_view_reply(&body).map_or(0, |reply| reply.view),
            Err(code) => {
                self.transport.close_channel(kept);
                return Err(match -code {
                    errno::ENOENT | errno::EPIPE => Error::NotRunning,
                    other => Error::Refused(other),
                });
            }
        };
        if view == 0 {
            self.transport.close_channel(kept);
            return Err(Error::Refused(errno::EINVAL));
        }
        self.live = Some(Live { view, events: kept });
        self.lost = None;
        self.size = (width, height);
        self.page = FullState::default();
        self.scroll = (0, 0, false);
        self.base = false;
        self.last_seq = 0;
        if let Err(error) = self.attach_slots() {
            self.teardown();
            return Err(error);
        }
        if !self.visible {
            self.set_visible(false);
        }
        if self.focused {
            self.focus(true);
        }
        Ok(())
    }

    /// End the view and forget its picture.
    pub fn destroy(&mut self) {
        self.teardown();
        self.picture = Picture::default();
        self.page = FullState::default();
    }

    /// Change the view's size: webd detaches every slot, so new ones are attached; the picture
    /// stays on show until the first whole frame of the new size.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), Error> {
        let view = self.live_view()?;
        if (width, height) == self.size {
            return Ok(());
        }
        let args = wire::ResizeArgs {
            view,
            width,
            height,
        };
        let body = wire::encode_resize_args(&args).map_err(|_| Error::Refused(errno::EINVAL))?;
        self.call(wire::METHOD_RESIZE, body)?;
        self.size = (width, height);
        self.slots.clear();
        self.owed.forget_slots();
        self.base = false;
        self.attach_slots()
    }

    /// Allocate and attach the slots for the current size.
    fn attach_slots(&mut self) -> Result<(), Error> {
        let view = self.live_view()?;
        let (width, height) = self.size;
        let frame = width as usize * height as usize * 4;
        let band = width as usize * BAND_ROWS as usize * 4;
        if frame == 0 {
            return Err(Error::Refused(errno::EINVAL));
        }
        let count = if self.config.deltas {
            ALL_SLOTS
        } else {
            FRAME_SLOTS
        };
        self.slots = (0..ALL_SLOTS).map(|_| None).collect();
        for slot in 0..count {
            let bytes = if slot < FRAME_SLOTS { frame } else { band };
            let mem = self
                .transport
                .alloc(bytes)
                .map_err(|code| Error::Refused(-code))?;
            let args = wire::AttachSlotArgs {
                view,
                slot: slot as u32,
                pixels: Buffer::whole(mem.handle(), bytes as u64),
            };
            let (body, objects) =
                wire::encode_attach_slot_args(&args).map_err(|_| Error::Refused(errno::EINVAL))?;
            self.call_with(wire::METHOD_ATTACHSLOT, body, objects)?;
            self.slots[slot] = Some(mem);
        }
        Ok(())
    }

    /// The id of the live view, or why there is none.
    fn live_view(&self) -> Result<u64, Error> {
        if let Some(lost) = self.lost {
            return Err(Error::Lost(lost));
        }
        self.live
            .as_ref()
            .map(|live| live.view)
            .ok_or(Error::NoView)
    }

    fn call(&mut self, method: u32, body: Vec<u8>) -> Result<(), Error> {
        self.call_with(method, body, Vec::new())
    }

    fn call_with(
        &mut self,
        method: u32,
        body: Vec<u8>,
        objects: Vec<libmessenger::Object>,
    ) -> Result<(), Error> {
        match self.transport.call(method, body, objects) {
            Ok(_) => Ok(()),
            Err(code) => Err(self.failed(-code)),
        }
    }

    /// A call about something of the view's that may be gone by itself (a held navigation expires
    /// after 10 seconds): webd answers `ESRCH` for a request it no longer holds, a refusal that
    /// leaves the view.
    fn call_about_request(&mut self, method: u32, body: Vec<u8>) -> Result<(), Error> {
        match self.transport.call(method, body, Vec::new()) {
            Ok(_) => Ok(()),
            Err(code) if -code == errno::ESRCH => Err(Error::Refused(errno::ESRCH)),
            Err(code) => Err(self.failed(-code)),
        }
    }

    /// What a failed call means: a service or view that is gone loses the view, a refusal leaves it.
    fn failed(&mut self, errno: i64) -> Error {
        let lost = match errno {
            errno::ENOENT | errno::EPIPE => Lost::ServiceGone,
            errno::ENODEV => Lost::Engine(wire::LOST_REASON_UNKNOWN),
            other => return Error::Refused(other),
        };
        Error::Lost(*self.lost.get_or_insert(lost))
    }

    fn lose(&mut self, why: Lost, out: &mut Vec<FullEvent>) {
        if self.lost.is_none() {
            self.lost = Some(why);
            out.push(FullEvent::Lost(why));
        }
    }

    /// End the view at webd (best effort), close our end of its events and drop the slots.
    fn teardown(&mut self) {
        if let Some(live) = self.live.take() {
            if self.lost != Some(Lost::ServiceGone) {
                let args = wire::DestroyViewArgs { view: live.view };
                if let Ok(body) = wire::encode_destroy_view_args(&args) {
                    let _ = self
                        .transport
                        .call(wire::METHOD_DESTROYVIEW, body, Vec::new());
                }
            }
            self.transport.close_channel(live.events);
        }
        self.slots.clear();
        self.owed.clear();
        self.lost = None;
        self.base = false;
    }

    pub fn view_id(&self) -> Option<u64> {
        self.live.as_ref().map(|live| live.view)
    }

    /// Our end of the view's event channel, to park on.
    pub fn events_handle(&self) -> Option<u64> {
        self.live.as_ref().map(|live| live.events)
    }

    pub fn is_live(&self) -> bool {
        self.live.is_some() && self.lost.is_none()
    }

    pub fn lost(&self) -> Option<Lost> {
        self.lost
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn config(&self) -> &ViewConfig {
        &self.config
    }

    /// The picture on show; empty before the first frame.
    pub fn picture(&self) -> &Picture {
        &self.picture
    }

    /// The page as the last `State` said it.
    pub fn page(&self) -> &FullState {
        &self.page
    }

    /// The scroll offset of the last frame or `Scroll`: `(x, y, at_bottom)`.
    pub fn scroll(&self) -> (i32, i32, bool) {
        self.scroll
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn counters(&self) -> FullCounters {
        self.counters
    }

    /// The transport (tests inspect their fabric through it).
    pub fn transport(&mut self) -> &mut T {
        &mut self.transport
    }
}

impl<T: ScopedTransport> Drop for FullWebView<T> {
    fn drop(&mut self) {
        self.teardown();
    }
}
