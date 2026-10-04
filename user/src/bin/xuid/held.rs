//! Input that arrives while a window animation plays.
//!
//! A zoom (`anim.rs`) is a short blocking loop of frames, so the compositor
//! does not get back to its main loop until it ends. Without help the cursor
//! froze for the whole minimize/restore/maximize/open animation. Each frame
//! therefore reads the pending input itself (the kernel display queue, and
//! `inputd`'s pointer when it owns the pointer) into [`HeldInput`]: every
//! event is kept, in arrival order, for the main loop to handle as soon as
//! the animation is over, and the newest pointer position is tracked so the
//! frame draws the cursor where the user's hand is.
//!
//! The queue is allocated once with a fixed capacity, since the user bump
//! allocator never reclaims. When it is full the remaining input simply stays
//! in its source queue, which the main loop drains after the held events, so
//! nothing is lost or reordered.

use alloc::vec::Vec;
use user::sys;

use super::compositor::Compositor;
use super::protocol::{decode_event, push_coalesced, Event, EventKind};

/// Events the held queue can keep: one full drain of the kernel queue.
pub(super) const CAPACITY: usize = 256;
/// Kernel input records fetched per `display_input_poll` call.
pub(super) const INPUT_BATCH: usize = 32;
/// The size of one kernel input record.
const EVENT_BYTES: usize = 16;

/// The events read during an animation, oldest first, and the pointer
/// position they lead to.
pub(super) struct HeldInput {
    queue: Vec<Event>,
    /// The newest pointer position read, while it differs from what the main
    /// loop has handled; the cursor is drawn here.
    pub(super) cursor: Option<(i32, i32)>,
}

impl HeldInput {
    pub(super) fn new() -> HeldInput {
        HeldInput {
            queue: Vec::with_capacity(CAPACITY),
            cursor: None,
        }
    }

    /// Free slots left (a coalesced move may take none).
    pub(super) fn room(&self) -> usize {
        CAPACITY - self.queue.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Keep `event` after everything already held, collapsing a run of moves
    /// like the main loop does. `false` (and nothing kept) when full.
    pub(super) fn push(&mut self, event: Event) -> bool {
        let coalesces = event.kind == EventKind::PointerMove
            && self
                .queue
                .last()
                .is_some_and(|last| last.kind == EventKind::PointerMove);
        if !coalesces && self.queue.len() >= CAPACITY {
            return false;
        }
        push_coalesced(&mut self.queue, event);
        if event.kind == EventKind::PointerMove {
            self.cursor = Some((event.a as i32, event.b as i32));
        }
        true
    }

    /// The oldest held event.
    pub(super) fn pop(&mut self) -> Option<Event> {
        (!self.queue.is_empty()).then(|| self.queue.remove(0))
    }

    /// Where the cursor is drawn: the newest held position, else `handled`.
    pub(super) fn pointer(&self, handled: (i32, i32)) -> (i32, i32) {
        self.cursor.unwrap_or(handled)
    }
}

/// Read kernel input records into `push` until the queue is empty or
/// `max_records` were read (rounded down to whole batches). While `inputd`
/// owns the pointer the kernel's pointer records are read and dropped, so
/// only keys come from this stream.
pub(super) fn drain_kernel(inputd_pointer: bool, max_records: usize, mut push: impl FnMut(Event)) {
    let mut records = [0u8; EVENT_BYTES * INPUT_BATCH];
    let mut read = 0;
    while read + INPUT_BATCH <= max_records {
        let Ok(count) = sys::display_input_poll(&mut records) else {
            break;
        };
        if count == 0 {
            break;
        }
        read += count;
        for index in 0..count {
            if let Some(event) = decode_event(&records, index) {
                if !(inputd_pointer && event.kind.is_pointer()) {
                    push(event);
                }
            }
        }
    }
}

impl Compositor {
    /// Handle `event` now, or hold it behind the events already held so the
    /// order the user produced them in is kept.
    pub(super) fn dispatch(&mut self, event: Event) {
        if self.held.is_empty() || !self.held.push(event) {
            self.handle_event(event);
        }
    }

    /// One animation frame's input: hold what the kernel queue and `inputd`
    /// delivered since the last frame.
    pub(super) fn hold_pending_input(&mut self) {
        let inputd_pointer = self.input.owns_pointer;
        let room = self.held.room();
        let held = &mut self.held;
        drain_kernel(inputd_pointer, room, |event| {
            held.push(event);
        });
        if inputd_pointer {
            self.hold_inputd_pointer();
        }
    }

    /// Handle every held event, oldest first, then drop the cursor override,
    /// repainting the cursor if it ends somewhere else. A held event may start
    /// another animation, which holds newer input behind the rest.
    pub(super) fn handle_held(&mut self) {
        while let Some(event) = self.held.pop() {
            self.handle_event(event);
        }
        if self.held.cursor.take().is_some() {
            self.move_cursor();
        }
    }
}

/// Boot check of the held queue: order, move coalescing, the cursor and the
/// capacity bound. `XUID:HELD:PASS` or `XUID:HELD:FAIL`.
pub(super) fn selftest_held() -> &'static str {
    let event = |kind, a, b| Event { kind, a, b };
    let mut held = HeldInput::new();
    let empty = held.pointer((1, 2)) == (1, 2) && held.pop().is_none();
    // A press between moves splits the run; the cursor is the newest move.
    held.push(event(EventKind::PointerMove, 5, 5));
    held.push(event(EventKind::PointerMove, 6, 7));
    held.push(event(EventKind::PointerDown, 1, 0));
    held.push(event(EventKind::KeyDown, 65, 0));
    held.push(event(EventKind::PointerMove, 9, 9));
    let cursor = held.pointer((0, 0)) == (9, 9);
    let order = [
        (EventKind::PointerMove, 6),
        (EventKind::PointerDown, 1),
        (EventKind::KeyDown, 65),
        (EventKind::PointerMove, 9),
    ]
    .iter()
    .all(|&(kind, a)| held.pop().is_some_and(|e| e.kind == kind && e.a == a))
        && held.is_empty();
    // Full: a key is refused, but a move still coalesces into a held move.
    let mut full = HeldInput::new();
    for index in 0..CAPACITY {
        full.push(event(EventKind::KeyDown, index as i64, 0));
    }
    let refused = !full.push(event(EventKind::KeyUp, 0, 0)) && full.room() == 0;
    full.pop();
    full.push(event(EventKind::PointerMove, 1, 1));
    let coalesced = full.push(event(EventKind::PointerMove, 2, 2)) && full.room() == 0;
    // The queue never reallocates: the bump allocator would leak the old one.
    let bounded = full.queue.capacity() == CAPACITY;
    if empty && cursor && order && refused && coalesced && bounded {
        "XUID:HELD:PASS\n"
    } else {
        "XUID:HELD:FAIL\n"
    }
}
