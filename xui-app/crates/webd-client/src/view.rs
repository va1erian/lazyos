//! The light view: create it, give webd slots to paint into, load a page, and turn the events that
//! come back into a picture and a few facts about the page.
//!
//! One rule keeps the slots simple: **a slot is webd's until the client has copied the frame out of
//! it**. [`LightWebView::poll`] reads every waiting event, copies only the newest frame into the
//! persistent [`Picture`] (the frames before it are stale by definition: each is a whole frame),
//! and releases every slot it was handed. A client that does not poll loses frames, never the
//! picture: webd drops what it cannot place and sends a fresh one when a slot comes back.

use libmessenger::Buffer;
use messenger_generated::os_lazy_webd_light_v1 as wire;

use crate::event::{self, Error, Lost, PageState, ViewEvent, Wire};
use crate::picture::Picture;
use crate::transport::{errno, SharedMem, Transport};

pub use wire::LightConfig;

/// Slots a view attaches by default: one on screen being copied out of, one being painted, one spare.
pub const SLOTS: usize = 3;
/// Most events one [`LightWebView::poll`] reads, so a flood cannot hold the UI thread.
pub const EVENTS_PER_POLL: usize = 256;

struct Slot<M> {
    mem: M,
    /// webd painted a frame into it and the client has not released it yet.
    held: bool,
}

pub(crate) struct Live {
    view: u64,
    /// Where the events arrive (our end of the pair `CreateView` moved the other end of).
    events: u64,
}

/// What the client has done, for traces and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Frames copied into the picture.
    pub applied: u64,
    /// Frames that arrived but were skipped because a newer one arrived with them.
    pub skipped: u64,
    /// Frames refused as stale (a size the view no longer has) and events that did not decode.
    pub ignored: u64,
}

/// One light view of `webd`, over a [`Transport`].
pub struct LightWebView<T: Transport> {
    pub(crate) transport: T,
    config: LightConfig,
    size: (u32, u32),
    pub(crate) live: Option<Live>,
    pub(crate) lost: Option<Lost>,
    slots: Vec<Slot<T::Mem>>,
    slot_count: usize,
    picture: Picture,
    page: PageState,
    scroll: (i32, i32, bool),
    /// The newest frame number applied to the picture; a new view counts from 1 again.
    last_seq: u64,
    /// The last page loaded and its base URL, to load again into a new view.
    html: Option<(Vec<u8>, String)>,
    pub(crate) visible: bool,
    pub(crate) focused: bool,
    counters: Counters,
}

impl<T: Transport> LightWebView<T> {
    /// A view that is not created yet; `config` is what [`LightWebView::create`] asks for.
    pub fn new(transport: T, config: LightConfig) -> Self {
        LightWebView {
            transport,
            config,
            size: (0, 0),
            live: None,
            lost: None,
            slots: Vec::new(),
            slot_count: SLOTS,
            picture: Picture::default(),
            page: PageState::default(),
            scroll: (0, 0, false),
            last_seq: 0,
            html: None,
            visible: true,
            focused: false,
            counters: Counters::default(),
        }
    }

    /// Attach `count` slots (2 or 3) to every view made from now on.
    pub fn with_slots(mut self, count: usize) -> Self {
        self.slot_count = count.clamp(1, SLOTS);
        self
    }

    /// Ask webd for a view of `width` x `height` pixels and attach its slots. A view that already
    /// exists is destroyed first. Fails with [`Error::NotRunning`] when `webd` is not there.
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
        let reply = self.transport.call(wire::METHOD_CREATEVIEW, body, objects);
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
        self.page = PageState::default();
        self.scroll = (0, 0, false);
        self.last_seq = 0;
        if let Err(error) = self.attach_slots() {
            self.teardown();
            return Err(error);
        }
        // A new view starts visible and unfocused: tell webd what the app last said.
        if !self.visible {
            self.set_visible(false);
        }
        if self.focused {
            self.focus(true);
        }
        Ok(())
    }

    /// Make a new view of the same size and config and load the last page into it, after the old
    /// one was lost (or any time): the picture on show stays until the new view paints.
    pub fn recreate(&mut self) -> Result<(), Error> {
        let (width, height) = self.size;
        self.create(width, height)?;
        if let Some((html, base)) = self.html.clone() {
            self.load(html, base)?;
        }
        Ok(())
    }

    /// End the view and forget its picture. The page last loaded is kept for [`LightWebView::recreate`].
    pub fn destroy(&mut self) {
        self.teardown();
        self.picture = Picture::default();
        self.page = PageState::default();
    }

    /// Replace the page with `html` at `base_url` (empty, `about:blank`, `about:...` or `data:`).
    pub fn set_html(&mut self, html: &str, base_url: &str) -> Result<(), Error> {
        self.load(html.as_bytes().to_vec(), base_url.to_owned())
    }

    /// Change the view's size. webd detaches every slot, so new ones are attached; the picture
    /// stays on show at its own size until the first frame of the new size.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), Error> {
        let view = self.live_view()?;
        if (width, height) == self.size {
            return Ok(());
        }
        let body = wire::encode_resize_args(&wire::ResizeArgs {
            view,
            width,
            height,
        })
        .map_err(|_| Error::Refused(errno::EINVAL))?;
        self.call(wire::METHOD_RESIZE, body)?;
        self.size = (width, height);
        // The old slots are the wrong size and webd no longer has them.
        self.slots.clear();
        self.attach_slots()
    }

    /// Read what has arrived, without parking: apply the newest frame to the picture, release the
    /// slots, and report what happened.
    pub fn poll(&mut self) -> Vec<ViewEvent> {
        let mut out = Vec::new();
        let Some(events) = self.live.as_ref().map(|live| live.events) else {
            return out;
        };
        let mut frames: Vec<wire::FrameArgs> = Vec::new();
        for _ in 0..EVENTS_PER_POLL {
            match self.transport.recv(events) {
                Ok(Some((method, body))) => self.event(method, &body, &mut frames, &mut out),
                Ok(None) => break,
                Err(_) => {
                    self.lose(Lost::ServiceGone, &mut out);
                    break;
                }
            }
        }
        self.apply_frames(frames, &mut out);
        out
    }

    fn event(
        &mut self,
        method: u32,
        body: &[u8],
        frames: &mut Vec<wire::FrameArgs>,
        out: &mut Vec<ViewEvent>,
    ) {
        let Some(mine) = self.live.as_ref().map(|live| live.view) else {
            return;
        };
        let Some(decoded) = event::decode(method, body) else {
            self.counters.ignored += 1;
            return;
        };
        match decoded {
            Wire::Frame(frame) if frame.view == mine => self.take_frame(frame, frames),
            Wire::State(state) if state.view == mine => {
                self.page = PageState {
                    title: state.title,
                    loading: state.loading,
                    finished: state.finished,
                    error: state.error,
                };
                out.push(ViewEvent::State(self.page.clone()));
            }
            Wire::Scroll(scroll) if scroll.view == mine => {
                self.scroll = (scroll.x, scroll.y, scroll.at_bottom);
                out.push(ViewEvent::Scroll {
                    x: scroll.x,
                    y: scroll.y,
                    at_bottom: scroll.at_bottom,
                });
            }
            Wire::Link(link) if link.view == mine => out.push(ViewEvent::LinkClicked(link.url)),
            Wire::Clipboard(copy) if copy.view == mine => {
                out.push(ViewEvent::ClipboardWrite(copy.text));
            }
            Wire::Lost(lost) if lost.view == mine => {
                self.lose(Lost::Engine(lost.reason), out);
            }
            // A ping is read by being read; an event for another view is not ours.
            _ => {}
        }
    }

    /// Note a frame handed to us. A frame that does not fit the slot we hold under that number
    /// (the size changed since, or the number is wrong) is not copied, and not released: webd does
    /// not have that slot out any more (`Resize` detaches them all).
    fn take_frame(&mut self, frame: wire::FrameArgs, frames: &mut Vec<wire::FrameArgs>) {
        let needed = u128::from(frame.width) * u128::from(frame.height) * 4;
        let fits = self.slots.get(frame.slot as usize).is_some_and(|slot| {
            (frame.width, frame.height) == self.size && needed <= slot.mem.len() as u128
        });
        if !fits || frame.width == 0 || frame.height == 0 {
            self.counters.ignored += 1;
            return;
        }
        self.slots[frame.slot as usize].held = true;
        frames.push(frame);
    }

    /// Copy the newest of `frames` into the picture and give every slot back.
    fn apply_frames(&mut self, frames: Vec<wire::FrameArgs>, out: &mut Vec<ViewEvent>) {
        let Some(newest) = frames.iter().max_by_key(|frame| frame.seq).cloned() else {
            return;
        };
        if newest.seq > self.last_seq {
            self.last_seq = newest.seq;
            let size = newest.width as usize * newest.height as usize * 4;
            let slot = &self.slots[newest.slot as usize];
            let into = self.picture.begin(newest.width, newest.height, newest.seq);
            slot.mem.read(&mut into[..size]);
            self.counters.applied += 1;
            self.counters.skipped += frames.len() as u64 - 1;
            out.push(ViewEvent::Frame {
                seq: newest.seq,
                width: newest.width,
                height: newest.height,
            });
        } else {
            self.counters.skipped += frames.len() as u64;
        }
        for frame in &frames {
            self.release(frame.slot);
        }
    }

    fn release(&mut self, slot: u32) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        if let Some(held) = self.slots.get_mut(slot as usize) {
            held.held = false;
        }
        let view = live.view;
        if let Ok(body) = wire::encode_release_slot_args(&wire::ReleaseSlotArgs { view, slot }) {
            self.send(wire::METHOD_RELEASESLOT, body);
        }
    }

    /// Allocate and attach the slots for the current size.
    fn attach_slots(&mut self) -> Result<(), Error> {
        let view = self.live_view()?;
        let (width, height) = self.size;
        let bytes = width as usize * height as usize * 4;
        if bytes == 0 {
            return Err(Error::Refused(errno::EINVAL));
        }
        for slot in 0..self.slot_count {
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
            self.slots.push(Slot { mem, held: false });
        }
        Ok(())
    }

    fn load(&mut self, html: Vec<u8>, base_url: String) -> Result<(), Error> {
        let view = self.live_view()?;
        // Shared memory cannot be empty; a blank page is a space.
        let bytes: &[u8] = if html.is_empty() { b" " } else { &html };
        let mut mem = self
            .transport
            .alloc(bytes.len())
            .map_err(|code| Error::Refused(-code))?;
        mem.write(bytes);
        let args = wire::LoadHtmlArgs {
            view,
            html: Buffer::whole(mem.handle(), bytes.len() as u64),
            base_url: base_url.clone(),
        };
        let (body, objects) =
            wire::encode_load_html_args(&args).map_err(|_| Error::Refused(errno::EINVAL))?;
        let result = self.call_with(wire::METHOD_LOADHTML, body, objects);
        // webd copied the bytes before it replied.
        drop(mem);
        match result {
            Err(Error::Refused(_)) => {}
            _ => self.html = Some((html, base_url)),
        }
        result
    }

    /// The id of the live view, or why there is none.
    pub(crate) fn live_view(&self) -> Result<u64, Error> {
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

    /// A oneway message. A service that is gone turns the view lost; nothing else can go wrong
    /// that the caller could do anything about.
    pub(crate) fn send(&mut self, method: u32, body: Vec<u8>) {
        if self.lost.is_some() {
            return;
        }
        if let Err(code) = self.transport.send(method, body) {
            if matches!(-code, errno::ENOENT | errno::EPIPE) {
                self.lost = Some(Lost::ServiceGone);
            }
        }
    }

    /// What a failed call means for the view: a service or view that is gone loses it, a refusal
    /// leaves it as it was.
    fn failed(&mut self, errno: i64) -> Error {
        let lost = match errno {
            errno::ENOENT | errno::EPIPE => Lost::ServiceGone,
            errno::ENODEV => Lost::Engine(wire::LOST_REASON_UNKNOWN),
            other => return Error::Refused(other),
        };
        Error::Lost(*self.lost.get_or_insert(lost))
    }

    fn lose(&mut self, why: Lost, out: &mut Vec<ViewEvent>) {
        if self.lost.is_none() {
            self.lost = Some(why);
            out.push(ViewEvent::Lost(why));
        }
    }

    /// End the view at webd (best effort), close our end of its events and drop the slots.
    fn teardown(&mut self) {
        if let Some(live) = self.live.take() {
            if self.lost != Some(Lost::ServiceGone) {
                if let Ok(body) =
                    wire::encode_destroy_view_args(&wire::DestroyViewArgs { view: live.view })
                {
                    let _ = self
                        .transport
                        .call(wire::METHOD_DESTROYVIEW, body, Vec::new());
                }
            }
            self.transport.close_channel(live.events);
        }
        self.slots.clear();
        self.lost = None;
    }

    pub fn view_id(&self) -> Option<u64> {
        self.live.as_ref().map(|live| live.view)
    }

    /// Our end of the view's event channel: what an app parks on (with its other handles) to wake
    /// when [`LightWebView::poll`] has something to read.
    pub fn events_handle(&self) -> Option<u64> {
        self.live.as_ref().map(|live| live.events)
    }

    /// The view exists and is not lost.
    pub fn is_live(&self) -> bool {
        self.live.is_some() && self.lost.is_none()
    }

    pub fn lost(&self) -> Option<Lost> {
        self.lost
    }

    /// The picture on show; empty before the first frame.
    pub fn picture(&self) -> &Picture {
        &self.picture
    }

    pub fn page(&self) -> &PageState {
        &self.page
    }

    /// Where the page is scrolled: `(x, y, at_bottom)`.
    pub fn scroll(&self) -> (i32, i32, bool) {
        self.scroll
    }

    /// The size webd was asked for.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// The transport, for a binding that needs to reach it (tests inspect their fabric through it).
    pub fn transport(&mut self) -> &mut T {
        &mut self.transport
    }
}

impl<T: Transport> Drop for LightWebView<T> {
    fn drop(&mut self) {
        self.teardown();
    }
}
