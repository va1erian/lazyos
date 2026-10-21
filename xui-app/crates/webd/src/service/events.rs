//! What flows to the clients: the engine's news turned into events on each view's endpoint, the
//! liveness `Ping`, and the end of a view whose client went away or whose engine side died.
//!
//! A send that fails with `EPIPE` means the client is gone (its endpoint closed with it): the view
//! is destroyed in the engine and forgotten here. `EAGAIN` means the client is slow and the event is
//! lost; for a frame that means the slot goes back to free and the picture is asked for again later,
//! for anything else the next state or scroll event carries the news. A full queue also puts the
//! view in back-off: no picture is asked for or copied for it for `FULL_BACKOFF_MS`, or until a send
//! to it gets through, so a client that stopped reading costs one frame in four per second, not
//! one per paint. The one event that must not be lost, `ViewLost`, is sent again until it arrives.

use messenger_generated::os_lazy_webd_light_v1 as wire;

use super::{Note, Service};
use crate::config::clip;
use crate::engine::{self, Engine};
use crate::events::{Drained, FrameNow, Once, StateNow};
use crate::fabric::{errno, Fabric};
use crate::limits::{EVENT_TEXT_BYTES, FULL_BACKOFF_MS, PING_INTERVAL_MS};

/// How an event send ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sent {
    Yes,
    /// The client's queue is full; the event is lost.
    Full,
    /// The client is gone; its view has been dropped.
    Gone,
}

impl<E: Engine, F: Fabric> Service<E, F> {
    /// One turn of the service thread's housekeeping: deliver what the engine said since the last
    /// turn, then ping the clients if one is due (`now_ms` is any monotonic clock), and ask for
    /// pictures that a full client queue lost.
    pub fn pump(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
        let drained = self.queue.drain();
        self.deliver(drained);
        // The first turn only starts the clock: a view made a moment ago needs no probe yet.
        let due = self
            .next_ping_ms
            .unwrap_or(now_ms.saturating_add(PING_INTERVAL_MS));
        if now_ms >= due {
            self.ping();
            self.next_ping_ms = Some(now_ms.saturating_add(PING_INTERVAL_MS));
        } else {
            self.next_ping_ms = Some(due);
        }
        self.retry_lost();
        self.retry_refreshes();
    }

    /// The engine itself is gone (its process exited): every view is lost, reason `Other`.
    pub fn engine_lost(&mut self) {
        for id in self.views.ids() {
            self.lose(id, wire::LOST_REASON_OTHER);
        }
    }

    /// End every view (the service is stopping). The engine is told; the clients' endpoints close.
    pub fn shutdown(&mut self) {
        for id in self.views.ids() {
            self.drop_view(id, "shutdown");
        }
    }

    fn deliver(&mut self, drained: Drained) {
        let Drained {
            states,
            scrolls,
            frames,
            once,
        } = drained;
        for (id, state) in states {
            self.deliver_state(id, state);
        }
        for (id, (x, y, at_bottom)) in scrolls {
            let body = wire::encode_scroll_args(&wire::ScrollArgs {
                view: id,
                x,
                y,
                at_bottom,
            });
            self.event_to_live(id, wire::METHOD_SCROLL, body);
        }
        for (id, frame) in frames {
            self.deliver_frame(id, frame);
        }
        for event in once {
            match event {
                Once::LinkClicked { view, url } => {
                    let url = clip(&url, EVENT_TEXT_BYTES).to_owned();
                    let body = wire::encode_link_clicked_args(&wire::LinkClickedArgs { view, url });
                    self.event_to_live(view, wire::METHOD_LINKCLICKED, body);
                }
                Once::ClipboardWrite { view, text } => {
                    let text = clip(&text, EVENT_TEXT_BYTES).to_owned();
                    let body =
                        wire::encode_clipboard_write_args(&wire::ClipboardWriteArgs { view, text });
                    self.event_to_live(view, wire::METHOD_CLIPBOARDWRITE, body);
                }
                Once::Lost { view, reason } => self.lose(view, lost_reason(reason)),
                Once::Say(line) => self.note(Note::Engine(line)),
            }
        }
    }

    fn deliver_state(&mut self, id: u64, state: StateNow) {
        let body = wire::encode_state_args(&wire::StateArgs {
            view: id,
            title: clip(&state.title, EVENT_TEXT_BYTES).to_owned(),
            loading: state.loading,
            finished: u32::try_from(state.finished).unwrap_or(0),
            error: clip(&state.error, EVENT_TEXT_BYTES).to_owned(),
        });
        self.event_to_live(id, wire::METHOD_STATE, body);
    }

    /// Send an event about `id` if the view is alive; its owner is whoever made it, the engine
    /// names views by id only.
    fn event_to_live(&mut self, id: u64, method: u32, body: Result<Vec<u8>, libmessenger::Error>) {
        let (Some(view), Ok(body)) = (self.views.any_mut(id), body) else {
            return;
        };
        if view.lost.is_some() {
            return;
        }
        let channel = view.channel;
        self.send(id, channel, method, body);
    }

    /// Put a whole frame into a free slot and tell the client; drop it when there is none.
    fn deliver_frame(&mut self, id: u64, frame: FrameNow) {
        let Some(view) = self.views.any_mut(id) else {
            return;
        };
        if view.lost.is_some() {
            return;
        }
        if view.backing_off(self.now_ms) {
            // The client's queue was full a moment ago: the copy would be lost again. The picture is
            // owed, and asked for once the wait is over.
            view.want_frame();
            self.note(Note::FrameDropped {
                view: id,
                why: "client backing off",
            });
            return;
        }
        let size_ok = u32::try_from(frame.width).is_ok_and(|w| w == view.width)
            && u32::try_from(frame.height).is_ok_and(|h| h == view.height)
            && frame.rgba.len() as u64 == crate::config::frame_bytes(view.width, view.height);
        if !size_ok {
            // Painted before a resize took effect: the picture of the new size comes next.
            self.note(Note::FrameDropped {
                view: id,
                why: "wrong size",
            });
            return;
        }
        let Some(slot) = view.slots.acquire() else {
            view.want_frame();
            self.note(Note::FrameDropped {
                view: id,
                why: "no free slot",
            });
            return;
        };
        match view.slots.memory_mut(slot) {
            Some(memory) if memory.capacity() >= frame.rgba.len() => memory.write(&frame.rgba),
            _ => {
                view.slots.release(slot);
                view.want_frame();
                return;
            }
        }
        view.seq += 1;
        let (channel, seq) = (view.channel, view.seq);
        let body = wire::encode_frame_args(&wire::FrameArgs {
            view: id,
            slot: slot as u32,
            seq,
            width: frame.width as u32,
            height: frame.height as u32,
        });
        let Ok(body) = body else {
            return;
        };
        match self.send(id, channel, wire::METHOD_FRAME, body) {
            Sent::Yes => {
                if let Some(view) = self.views.any_mut(id) {
                    view.frame_delivered();
                }
            }
            Sent::Full => {
                // The client never heard of the slot: it is still free, and the picture is owed.
                if let Some(view) = self.views.any_mut(id) {
                    view.slots.release(slot);
                    view.seq -= 1;
                    view.want_frame();
                }
                self.note(Note::FrameDropped {
                    view: id,
                    why: "client queue full",
                });
            }
            Sent::Gone => {}
        }
    }

    /// The engine gave `id` up (or is gone): tell the client once, free the slots, keep the entry.
    fn lose(&mut self, id: u64, reason: u32) {
        let Some(view) = self.views.any_mut(id) else {
            return;
        };
        if view.lost.is_some() {
            return;
        }
        view.lost = Some(reason);
        view.lost_unsent = true;
        view.slots.detach_all();
        self.note(Note::ViewLost { view: id, reason });
        self.tell_lost(id);
    }

    /// Send the `ViewLost` of a lost view the client has not been told about. A full queue leaves
    /// it owed: without it the client would wait forever for a picture of a view that is gone.
    fn tell_lost(&mut self, id: u64) {
        let Some(view) = self.views.any_mut(id) else {
            return;
        };
        let (Some(reason), true, channel) = (view.lost, view.lost_unsent, view.channel) else {
            return;
        };
        let Ok(body) = wire::encode_view_lost_args(&wire::ViewLostArgs { view: id, reason }) else {
            return;
        };
        if self.send(id, channel, wire::METHOD_VIEWLOST, body) == Sent::Yes {
            if let Some(view) = self.views.any_mut(id) {
                view.lost_unsent = false;
            }
        }
    }

    /// Tell the clients of the lost views that were not told yet, at every turn.
    fn retry_lost(&mut self) {
        for id in self.views.ids() {
            self.tell_lost(id);
        }
    }

    fn ping(&mut self) {
        for id in self.views.ids() {
            let Some(view) = self.views.any_mut(id) else {
                continue;
            };
            let channel = view.channel;
            self.send(id, channel, wire::METHOD_PING, Vec::new());
        }
    }

    /// Views whose last frame was lost to a full queue, or that were waiting for a slot.
    fn retry_refreshes(&mut self) {
        let now = self.now_ms;
        for id in self.views.ids() {
            if self
                .views
                .any_mut(id)
                .is_some_and(|view| !view.backing_off(now) && view.should_refresh())
            {
                self.engine.refresh(id);
            }
        }
    }

    /// Send an event; the result also keeps the view's back-off: a full queue starts it (no
    /// picture is asked for or copied for [`FULL_BACKOFF_MS`]) and a send that gets through ends it.
    fn send(&mut self, id: u64, channel: u64, method: u32, body: Vec<u8>) -> Sent {
        match self.fabric.send(channel, method, body) {
            Ok(()) => {
                if let Some(view) = self.views.any_mut(id) {
                    view.backoff_until = None;
                }
                Sent::Yes
            }
            Err(code) if -code == errno::EAGAIN => {
                let until = self.now_ms.saturating_add(FULL_BACKOFF_MS);
                if let Some(view) = self.views.any_mut(id) {
                    view.backoff_until = Some(until);
                }
                Sent::Full
            }
            Err(_) => {
                self.drop_view(id, "client gone");
                Sent::Gone
            }
        }
    }

    /// Remove a view: the engine forgets it (unless it already gave it up), the slots unmap, the
    /// client's endpoint closes. An unlabelled client with no view left loses its group number.
    pub(super) fn drop_view(&mut self, id: u64, why: &'static str) {
        let Some(view) = self.views.remove(id) else {
            return;
        };
        if view.lost.is_none() {
            self.engine.view_destroy(id);
        }
        self.fabric.close_channel(view.channel);
        let owner = view.owner;
        // Slots unmap when the view drops.
        drop(view);
        if self.views.count_of(owner) == 0 {
            self.groups.idle(owner);
        }
        self.note(Note::ViewEnded { view: id, why });
    }
}

/// The `LostReason` for the engine's `WEBD_LOST_*` value.
fn lost_reason(engine_reason: i32) -> u32 {
    match engine_reason {
        engine::LOST_REFUSED => wire::LOST_REASON_REFUSED,
        engine::LOST_CRASHED => wire::LOST_REASON_CRASHED,
        engine::LOST_MEMORY => wire::LOST_REASON_OUT_OF_MEMORY,
        engine::LOST_OTHER => wire::LOST_REASON_OTHER,
        _ => wire::LOST_REASON_UNKNOWN,
    }
}
