//! Reading a full view's events: frames applied to the picture (a whole frame copied, a delta
//! moved and patched), bands copied out, every slot released as soon as it was read, and the rest
//! decoded into [`FullEvent`]s.

use messenger_generated::os_lazy_webd_view_v1 as wire;

use super::event::{self, Band, FrameInfo, FullEvent, Wire};
use super::sink::PixelSink;
use super::{FullWebView, BAND_ROWS, FRAME_SLOTS};
use crate::event::Lost;
use crate::transport::{ScopedTransport, SharedMem};
use crate::view::EVENTS_PER_POLL;

/// Whether every rectangle of a delta lies inside a view of `size`.
fn delta_fits(rects: &[super::Rect], size: (u32, u32)) -> bool {
    let (w, h) = (i64::from(size.0), i64::from(size.1));
    rects.iter().all(|r| {
        let (x, y, rw, rh) = (
            i64::from(r.x),
            i64::from(r.y),
            i64::from(r.width),
            i64::from(r.height),
        );
        x >= 0 && y >= 0 && rw >= 0 && rh >= 0 && x + rw <= w && y + rh <= h
    })
}

impl<T: ScopedTransport> FullWebView<T> {
    /// Read what has arrived, without parking: apply each frame to the picture in order, copy the
    /// bands, release their slots, and report everything.
    pub fn poll(&mut self) -> Vec<FullEvent> {
        self.poll_with(None)
    }

    pub(super) fn poll_with(&mut self, sink: Option<&mut dyn PixelSink>) -> Vec<FullEvent> {
        let mut out = std::mem::take(&mut self.stashed);
        self.flush_owed();
        self.read_events(sink, &mut out);
        out
    }

    /// Read what is queued, up to [`EVENTS_PER_POLL`] events, into `out`. `true` when that many were
    /// read and more may wait.
    pub(super) fn read_events(
        &mut self,
        mut sink: Option<&mut dyn PixelSink>,
        out: &mut Vec<FullEvent>,
    ) -> bool {
        let Some(events) = self.live.as_ref().map(|live| live.events) else {
            return false;
        };
        for _ in 0..EVENTS_PER_POLL {
            match self.transport.recv(events) {
                Ok(Some((method, body))) => {
                    let sink: Option<&mut dyn PixelSink> = match &mut sink {
                        Some(sink) => Some(&mut **sink),
                        None => None,
                    };
                    self.event(method, &body, out, sink)
                }
                Ok(None) => return false,
                Err(_) => {
                    self.lose(Lost::ServiceGone, out);
                    return false;
                }
            }
            if self.lost.is_some() {
                return false;
            }
        }
        true
    }

    fn event(
        &mut self,
        method: u32,
        body: &[u8],
        out: &mut Vec<FullEvent>,
        sink: Option<&mut dyn PixelSink>,
    ) {
        let Some(mine) = self.view_id() else {
            return;
        };
        let Some(decoded) = event::decode(method, body) else {
            self.counters.ignored += 1;
            return;
        };
        match decoded {
            Wire::Frame(frame) if frame.view == mine => self.frame(frame, out, sink),
            Wire::Band(band) if band.view == mine => self.band(band, out),
            Wire::State(state) if state.view == mine => {
                self.page = event::state(state);
                out.push(FullEvent::State(self.page.clone()));
            }
            Wire::Lost(lost) if lost.view == mine => self.lose(Lost::Engine(lost.reason), out),
            Wire::Plain(view, plain) if view == mine => {
                if let FullEvent::Scroll { x, y, at_bottom } = plain {
                    self.scroll = (x, y, at_bottom);
                }
                out.push(plain);
            }
            // A ping is read by being read; an event for another view is not ours.
            _ => {}
        }
    }

    /// Apply a frame to the picture and give its slot back. A frame that does not fit the slot we
    /// hold under that number (the size changed since) is not copied and not released: webd does
    /// not have that slot out any more (`Resize` detaches them all).
    fn frame(
        &mut self,
        frame: wire::FrameArgs,
        out: &mut Vec<FullEvent>,
        sink: Option<&mut dyn PixelSink>,
    ) {
        let size = self.size;
        let bytes = u128::from(frame.width) * u128::from(frame.height) * 4;
        let index = frame.slot as usize;
        let held = (index < FRAME_SLOTS)
            .then(|| self.slots.get(index).and_then(Option::as_ref))
            .flatten();
        let fits = held.is_some_and(|mem| bytes <= mem.len() as u128)
            && (frame.width, frame.height) == size
            && frame.width > 0
            && frame.height > 0;
        if !fits {
            self.counters.ignored += 1;
            return;
        }
        let mut info = FrameInfo {
            seq: frame.seq,
            width: frame.width,
            height: frame.height,
            whole: frame.whole,
            shift: frame.shift.clone(),
            rects: frame.rects.clone(),
            offset: (frame.offset_x, frame.offset_y, frame.at_bottom),
            applied: false,
        };
        let applied = match sink {
            Some(sink) => self.to_sink(&info, frame.slot, sink),
            None if frame.whole => {
                self.copy_whole(&frame);
                true
            }
            None => self.apply_delta(&frame),
        };
        info.applied = applied;
        if applied {
            self.last_seq = frame.seq;
            self.scroll = (frame.offset_x, frame.offset_y, frame.at_bottom);
        } else {
            // The picture is not what the delta was made against: ask for a whole one.
            self.base = false;
            self.counters.ignored += 1;
            self.counters.refreshes += 1;
            self.refresh();
        }
        self.owe_release(frame.slot);
        out.push(FullEvent::Frame(info));
    }

    fn copy_whole(&mut self, frame: &wire::FrameArgs) {
        let bytes = frame.width as usize * frame.height as usize * 4;
        let into = self.picture.begin(frame.width, frame.height, frame.seq);
        if let Some(Some(mem)) = self.slots.get(frame.slot as usize) {
            mem.read(&mut into[..bytes]);
        }
        self.base = true;
        self.drawn = (frame.width, frame.height);
        self.counters.whole += 1;
    }

    /// Whether a delta of `seq` at `size` follows the frame the picture (or the sink) shows.
    fn has_base(&self, seq: u64, size: (u32, u32)) -> bool {
        self.base && seq == self.last_seq + 1 && self.drawn == size
    }

    /// Hand a checked frame to the app's sink; `false` for a delta it cannot apply.
    fn to_sink(&mut self, info: &FrameInfo, slot: u32, sink: &mut dyn PixelSink) -> bool {
        let Some(Some(mem)) = self.slots.get(slot as usize) else {
            return false;
        };
        if info.whole {
            sink.whole(info, mem);
            self.base = true;
            self.drawn = (info.width, info.height);
            self.counters.whole += 1;
            return true;
        }
        let fits = delta_fits(&info.rects, self.size);
        if !fits || !self.has_base(info.seq, (info.width, info.height)) || !sink.delta(info, mem) {
            return false;
        }
        self.counters.deltas += 1;
        true
    }

    /// Move the picture by the frame's shift and copy its rectangles out of the slot; `false` when
    /// it cannot be (no base, a frame missed in between, a rectangle outside the view).
    fn apply_delta(&mut self, frame: &wire::FrameArgs) -> bool {
        let base = self.has_base(frame.seq, (frame.width, frame.height))
            && (self.picture.width(), self.picture.height()) == (frame.width, frame.height);
        if !base || !delta_fits(&frame.rects, self.size) {
            return false;
        }
        if let Some(shift) = &frame.shift {
            let area = [shift.x, shift.y, shift.width, shift.height];
            self.picture.shift(area, shift.dx, shift.dy);
        }
        let stride = frame.width as usize * 4;
        let Some(Some(mem)) = self.slots.get(frame.slot as usize) else {
            return false;
        };
        let pixels = self.picture.patch(frame.seq);
        for rect in &frame.rects {
            let (x, y) = (rect.x as usize, rect.y as usize);
            let row_bytes = rect.width as usize * 4;
            for row in y..y + rect.height as usize {
                let at = row * stride + x * 4;
                mem.read_at(at, &mut pixels[at..at + row_bytes]);
            }
        }
        self.counters.deltas += 1;
        true
    }

    /// Copy a band out of its slot and give the slot back. A band that does not fit its slot (a
    /// stale width) is dropped, and released: band slots are not detached by anything but `Resize`.
    fn band(&mut self, band: wire::BandArgs, out: &mut Vec<FullEvent>) {
        let index = band.slot as usize;
        let bytes = band.width as usize * band.height.min(BAND_ROWS) as usize * 4;
        let mem = (index >= FRAME_SLOTS)
            .then(|| self.slots.get(index).and_then(Option::as_ref))
            .flatten();
        let Some(mem) = mem else {
            self.counters.ignored += 1;
            return;
        };
        let fits = band.width == self.size.0 && band.height <= BAND_ROWS && bytes <= mem.len();
        if fits && bytes > 0 {
            let mut rgba = vec![0; bytes];
            mem.read(&mut rgba);
            self.counters.bands += 1;
            out.push(FullEvent::Band(Band {
                doc_x: band.doc_x,
                doc_y: band.doc_y,
                width: band.width,
                height: band.height,
                rgba,
            }));
        } else {
            self.counters.ignored += 1;
        }
        self.owe_release(band.slot);
    }
}
