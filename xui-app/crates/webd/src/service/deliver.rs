//! What flows to a full view's client beyond a light view's events: its page state, the pictures of a
//! view with deltas (a whole frame, then deltas, each in a slot of its own) and bands, the link under
//! the pointer, the pointer's shape, responsiveness, blocked counts, and the page's news and
//! questions (`Once`), some of which the service answers itself (a dialog over the rate limit, a held
//! navigation over the per-view count, a hand-off).
//!
//! **Pictures.** A delta only means something on top of the picture the client has, so the first
//! picture the client cannot get (no free slot, a full queue, a stale size) ends the chain: the rest
//! of the turn's deltas are dropped, the queue refuses further deltas until a whole frame
//! (`EventQueue::lose_base`, the engine then paints whole), and the view asks for one when a slot is
//! free, as a light view does for any lost frame.

use std::collections::BTreeMap;

use messenger_generated::os_lazy_webd_view_v1 as wire;

use super::events::Sent;
use super::frame_args::frame_args;
use super::{Note, Service};
use crate::config::{clip, frame_bytes};
use crate::engine::Engine;
use crate::events::pictures::{DeltaNow, Update};
use crate::events::{BandNow, Once, PageNow};
use crate::fabric::Fabric;
use crate::limits::{
    DIALOGS_PER_WINDOW, DIALOG_WINDOW_MS, EVENT_TEXT_BYTES, HELD_PER_VIEW, HOLD_MS,
};
use crate::view::Held;

fn text(value: &str) -> String {
    clip(value, EVENT_TEXT_BYTES).to_owned()
}

/// A full `State` carries four texts where the light one has two: each is cut at half the event
/// text limit so the event fits the receive buffer a client offers (`EVENT_BYTES_MAX`).
fn state_text(value: &str) -> String {
    clip(value, EVENT_TEXT_BYTES / 2).to_owned()
}

impl<E: Engine, F: Fabric> Service<E, F> {
    pub(super) fn deliver_pages(&mut self, pages: BTreeMap<u64, PageNow>) {
        for (id, page) in pages {
            if let Some(full) = self.full_of(id) {
                full.committed.clone_from(&page.committed_url);
            }
            let body = wire::encode_state_args(&wire::StateArgs {
                view: id,
                title: state_text(&page.title),
                loading: page.loading,
                finished: page.finished,
                error: state_text(&page.error),
                url: state_text(&page.url),
                committed_url: state_text(&page.committed_url),
                can_back: page.can_back,
                can_forward: page.can_forward,
                progress: page.progress,
                secure: page.secure,
            });
            self.event_to_live(id, wire::METHOD_STATE, body);
        }
    }

    /// A view's pictures of this turn: the whole frame first, then the deltas, in order.
    pub(super) fn deliver_update(&mut self, id: u64, update: Update) {
        if let Some((frame, offset)) = update.whole {
            if !self.deliver_frame(id, frame, offset) {
                self.queue.lose_base(id);
                return;
            }
        }
        let mut deltas = update.deltas.into_iter();
        for delta in deltas.by_ref() {
            let delivered = self.deliver_delta(id, &delta);
            self.recycle_delta(delta);
            if !delivered {
                // The client's picture misses this one: no later delta can apply to it.
                self.queue.lose_base(id);
                if let Some(view) = self.views.any_mut(id) {
                    view.want_frame();
                }
                break;
            }
        }
        for delta in deltas {
            self.recycle_delta(delta);
        }
    }

    /// A delta's pixels, written or not, are the engine's next copies (`events/spares.rs`).
    fn recycle_delta(&mut self, delta: DeltaNow) {
        self.spent
            .extend(delta.patches.into_iter().map(|patch| patch.rgba));
    }

    /// One delta into a free slot (only its rectangles are written, at their place); `false` when it
    /// could not be delivered.
    fn deliver_delta(&mut self, id: u64, delta: &DeltaNow) -> bool {
        let now = self.now_ms;
        let Some(view) = self.views.any_mut(id) else {
            return false;
        };
        let size = (view.width, view.height);
        let fits = view.lost.is_none()
            && !view.backing_off(now)
            && u32::try_from(delta.width).ok() == Some(size.0)
            && u32::try_from(delta.height).ok() == Some(size.1);
        if !fits {
            return false;
        }
        let Some(slot) = view.slots.acquire() else {
            self.note(Note::FrameDropped {
                view: id,
                why: "no free slot",
            });
            return false;
        };
        let stride = size.0 as usize * 4;
        let needed = frame_bytes(size.0, size.1) as usize;
        match view.slots.memory_mut(slot) {
            Some(memory) if memory.capacity() >= needed => {
                for patch in &delta.patches {
                    let [x, y, w, _] = patch.rect;
                    for (row, line) in patch.rgba.chunks_exact(w.max(1) as usize * 4).enumerate() {
                        memory.write_at((y as usize + row) * stride + x as usize * 4, line);
                    }
                }
            }
            _ => {
                view.slots.release(slot);
                return false;
            }
        }
        view.seq += 1;
        let (channel, seq) = (view.channel, view.seq);
        let args = frame_args(id, slot, seq, size, Some(delta), delta.offset);
        let Ok(body) = wire::encode_frame_args(&args) else {
            return false;
        };
        match self.send(id, channel, wire::METHOD_FRAME, body) {
            Sent::Yes => true,
            Sent::Full => {
                if let Some(view) = self.views.any_mut(id) {
                    view.slots.release(slot);
                    view.seq -= 1;
                }
                false
            }
            Sent::Gone => false,
        }
    }

    /// Bands into free band slots, newest last; one without a slot is dropped (they are speculative).
    pub(super) fn deliver_bands(&mut self, id: u64, bands: Vec<BandNow>) {
        for band in bands {
            self.deliver_band(id, &band);
            self.spent.push(band.rgba);
        }
    }

    /// One band into a free band slot; one without a slot is dropped.
    fn deliver_band(&mut self, id: u64, band: &BandNow) {
        let Some(view) = self.views.any_mut(id) else {
            return;
        };
        let bytes = band.rgba.len();
        if view.lost.is_some() || u32::try_from(band.width).ok() != Some(view.width) {
            return;
        }
        let Some(slot) = view.slots.acquire_band() else {
            return;
        };
        match view.slots.memory_mut(slot) {
            Some(memory) if memory.capacity() >= bytes => memory.write(&band.rgba),
            _ => {
                view.slots.release(slot);
                return;
            }
        }
        let channel = view.channel;
        let body = wire::encode_band_args(&wire::BandArgs {
            view: id,
            slot: slot as u32,
            doc_x: band.doc_x,
            doc_y: band.doc_y,
            width: band.width as u32,
            height: band.height as u32,
        });
        let Ok(body) = body else {
            return;
        };
        if self.send(id, channel, wire::METHOD_BAND, body) == Sent::Full {
            if let Some(view) = self.views.any_mut(id) {
                view.slots.release(slot);
            }
        }
    }

    /// The small latest-per-view news.
    pub(super) fn deliver_latest(
        &mut self,
        hovers: BTreeMap<u64, String>,
        cursors: BTreeMap<u64, i32>,
        responsive: BTreeMap<u64, bool>,
        blocked: BTreeMap<u64, i32>,
    ) {
        for (view, url) in hovers {
            let body = wire::encode_hover_args(&wire::HoverArgs {
                view,
                url: text(&url),
            });
            self.event_to_live(view, wire::METHOD_HOVER, body);
        }
        for (view, shape) in cursors {
            let shape = u32::try_from(shape).unwrap_or(0);
            let body = wire::encode_cursor_args(&wire::CursorArgs { view, shape });
            self.event_to_live(view, wire::METHOD_CURSOR, body);
        }
        for (view, responsive) in responsive {
            let body = wire::encode_responsive_args(&wire::ResponsiveArgs { view, responsive });
            self.event_to_live(view, wire::METHOD_RESPONSIVE, body);
        }
        for (view, count) in blocked {
            let count = u32::try_from(count).unwrap_or(0);
            let body = wire::encode_blocked_args(&wire::BlockedArgs { view, count });
            self.event_to_live(view, wire::METHOD_BLOCKED, body);
        }
    }

    /// A full view's news that happened once. Returns it back when it is not one of these.
    pub(super) fn full_once(&mut self, event: Once) -> Option<Once> {
        match event {
            Once::NewWindow { view, url, gesture } => {
                let wanted = self.full_of(view).is_some_and(|full| full.rules.popups);
                if wanted {
                    let body =
                        wire::encode_new_window_requested_args(&wire::NewWindowRequestedArgs {
                            view,
                            url,
                            gesture,
                        });
                    self.event_to_live(view, wire::METHOD_NEWWINDOWREQUESTED, body);
                }
            }
            Once::Held {
                view,
                request,
                url,
                kind,
                gesture,
            } => self.held(view, request, url, kind, gesture),
            Once::Superseded { view, request } => {
                self.engine.navigation_answer(view, request, false);
                self.note(Note::Full {
                    view,
                    what: "held superseded",
                });
            }
            Once::HandOff { view, url } => {
                let handed = self.outside.open_url(&url);
                self.note(Note::Full {
                    view,
                    what: if handed {
                        "hand-off"
                    } else {
                        "hand-off refused"
                    },
                });
                if handed {
                    let body = wire::encode_handed_off_args(&wire::HandedOffArgs { view, url });
                    self.event_to_live(view, wire::METHOD_HANDEDOFF, body);
                }
            }
            Once::Dialog {
                view,
                dialog,
                kind,
                message,
                default,
            } => self.dialog(view, dialog, kind, message, default),
            Once::DownloadStarted {
                view,
                download,
                url,
                name,
                total,
            } => {
                let args = wire::DownloadStartedArgs {
                    view,
                    download,
                    url,
                    name,
                    total,
                };
                let body = wire::encode_download_started_args(&args);
                self.event_to_live(view, wire::METHOD_DOWNLOADSTARTED, body);
            }
            Once::DownloadProgress {
                view,
                download,
                received,
                total,
            } => {
                let args = wire::DownloadProgressArgs {
                    view,
                    download,
                    received,
                    total,
                };
                let body = wire::encode_download_progress_args(&args);
                self.event_to_live(view, wire::METHOD_DOWNLOADPROGRESS, body);
            }
            Once::DownloadDone {
                view,
                download,
                name,
            } => {
                let args = wire::DownloadDoneArgs {
                    view,
                    download,
                    name,
                };
                let body = wire::encode_download_done_args(&args);
                self.event_to_live(view, wire::METHOD_DOWNLOADDONE, body);
            }
            Once::DownloadFailed {
                view,
                download,
                reason,
            } => {
                let args = wire::DownloadFailedArgs {
                    view,
                    download,
                    reason,
                };
                let body = wire::encode_download_failed_args(&args);
                self.event_to_live(view, wire::METHOD_DOWNLOADFAILED, body);
            }
            Once::BlockingList { view, list, status } => {
                let status = u32::try_from(status).unwrap_or(wire::BLOCKING_STATUS_UNAVAILABLE);
                let body =
                    wire::encode_blocking_list_args(&wire::BlockingListArgs { view, list, status });
                self.event_to_live(view, wire::METHOD_BLOCKINGLIST, body);
            }
            other => return self.notify_once(other),
        }
        None
    }

    /// A navigation the engine holds: the client is asked, unless the view holds too many already
    /// (refused at once). The pump refuses it when [`HOLD_MS`] pass without an answer.
    fn held(&mut self, view: u64, request: u64, url: String, kind: i32, gesture: bool) {
        let deadline_ms = self.now_ms.saturating_add(HOLD_MS);
        let loadable = self
            .full_of(view)
            .is_some_and(|full| full.rules.policy.may_load_when_asked(&url));
        let alive = self.views.any_mut(view).is_some_and(|v| v.lost.is_none());
        let kept = alive
            && self.full_of(view).is_some_and(|full| {
                full.held.len() < HELD_PER_VIEW && {
                    full.held.insert(
                        request,
                        Held {
                            deadline_ms,
                            loadable,
                        },
                    );
                    true
                }
            });
        if !kept {
            self.engine.navigation_answer(view, request, false);
            self.note(Note::Full {
                view,
                what: "held refused",
            });
            return;
        }
        self.note(Note::Full { view, what: "held" });
        let kind = u32::try_from(kind).unwrap_or(wire::NAVIGATION_KIND_OTHER);
        let args = wire::NavigationRequestedArgs {
            view,
            request,
            url,
            kind,
            gesture,
        };
        let body = wire::encode_navigation_requested_args(&args);
        self.event_to_live(view, wire::METHOD_NAVIGATIONREQUESTED, body);
    }

    /// Refuse the held navigations whose time ran out.
    pub(super) fn expire_held(&mut self) {
        let now = self.now_ms;
        for id in self.views.ids() {
            let expired: Vec<u64> = match self.full_of(id) {
                Some(full) => {
                    let gone: Vec<u64> = full
                        .held
                        .iter()
                        .filter(|(_, held)| held.deadline_ms <= now)
                        .map(|(request, _)| *request)
                        .collect();
                    gone.iter().for_each(|request| {
                        full.held.remove(request);
                    });
                    gone
                }
                None => Vec::new(),
            };
            for request in expired {
                self.engine.navigation_answer(id, request, false);
                self.note(Note::Full {
                    view: id,
                    what: "held expired",
                });
            }
        }
    }

    /// A JavaScript dialog: the client is asked, unless the page opened [`DIALOGS_PER_WINDOW`] in the
    /// last [`DIALOG_WINDOW_MS`] (dismissed at once: a page that loops `alert()` gets no further).
    fn dialog(&mut self, view: u64, dialog: u64, kind: i32, message: String, default: String) {
        let now = self.now_ms;
        let allowed = self.full_of(view).is_some_and(|full| {
            while full
                .dialogs
                .front()
                .is_some_and(|at| now.saturating_sub(*at) >= DIALOG_WINDOW_MS)
            {
                full.dialogs.pop_front();
            }
            full.dialogs.len() < DIALOGS_PER_WINDOW && {
                full.dialogs.push_back(now);
                true
            }
        });
        if !allowed {
            self.engine.dialog_reply(view, dialog, false, "");
            self.note(Note::Full {
                view,
                what: "dialog dismissed",
            });
            return;
        }
        let kind = u32::try_from(kind).unwrap_or(wire::DIALOG_KIND_ALERT);
        let args = wire::DialogArgs {
            view,
            dialog,
            kind,
            message,
            default_value: default,
        };
        self.event_to_live(view, wire::METHOD_DIALOG, wire::encode_dialog_args(&args));
    }

    /// The full part of a view that is alive or lost (the engine's news may come after a loss).
    pub(super) fn full_of(&mut self, id: u64) -> Option<&mut crate::view::FullView> {
        self.views.any_mut(id)?.full.as_deref_mut()
    }
}
