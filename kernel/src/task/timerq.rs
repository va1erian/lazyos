//! The timer queue (docs/performance-plan.md, P2.1): every blocked task's
//! deadline, in monotonic nanoseconds, ordered so the next one to expire is
//! found without looking at the others.
//!
//! A task has at most one deadline (it is blocked on one thing), so the queue
//! is an indexed binary min-heap over task slots: arming a slot that is
//! already queued moves its entry, cancelling removes it, and both are
//! `O(log n)` with no allocation. Expiry pops entries while the earliest is
//! due, so a scheduler entry with nothing due costs one comparison instead of
//! a scan of all `MAX_TASKS` slots.
//!
//! The queue is a hint the task table confirms: an entry popped for a slot
//! whose task is no longer blocked with that same deadline (it was woken,
//! stopped, finished or replaced since) is dropped. So the only invariant the
//! rest of the kernel keeps is "a task blocked with a deadline has an entry
//! with that deadline", which `block_task` establishes.
//!
//! Lock order: `TASKS`, then [`TIMERS`]. Every path that changes a task's
//! state already holds the task table, so the queue changes in the same
//! critical section.

use spin::Mutex;

use super::MAX_TASKS;

/// `pos` value of a slot with no entry.
const NONE: u16 = u16::MAX;

/// The kernel's timer queue (see the module docs for the lock order).
pub static TIMERS: Mutex<TimerQueue> = Mutex::new(TimerQueue::new());

/// An indexed min-heap of `(deadline, slot)`, at most one entry per slot.
pub struct TimerQueue {
    len: usize,
    /// Slots in heap order.
    heap: [u16; MAX_TASKS],
    /// Slot -> index in `heap`, or [`NONE`].
    pos: [u16; MAX_TASKS],
    /// Slot -> its deadline (meaningful while queued).
    key: [u64; MAX_TASKS],
}

impl TimerQueue {
    pub const fn new() -> Self {
        Self {
            len: 0,
            heap: [0; MAX_TASKS],
            pos: [NONE; MAX_TASKS],
            key: [0; MAX_TASKS],
        }
    }

    /// Entries queued.
    #[allow(dead_code)] // test hook
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing is queued.
    #[allow(dead_code)] // test hook
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// `slot`'s queued deadline, if any.
    #[allow(dead_code)] // test hook
    pub fn deadline_of(&self, slot: usize) -> Option<u64> {
        let queued = self.pos.get(slot).is_some_and(|&p| p != NONE);
        queued.then(|| self.key[slot])
    }

    /// Queue `slot` to expire at `deadline`, replacing an earlier entry for it.
    /// Out-of-range slots are ignored.
    pub fn arm(&mut self, slot: usize, deadline: u64) {
        if slot >= MAX_TASKS {
            return;
        }
        self.key[slot] = deadline;
        let at = match self.pos[slot] {
            NONE => {
                let at = self.len;
                self.heap[at] = slot as u16;
                self.pos[slot] = at as u16;
                self.len += 1;
                at
            }
            at => at as usize,
        };
        let at = self.sift_up(at);
        self.sift_down(at);
    }

    /// Remove `slot`'s entry. Returns whether it had one.
    pub fn cancel(&mut self, slot: usize) -> bool {
        match self.pos.get(slot) {
            Some(&at) if at != NONE => {
                self.remove_at(at as usize);
                true
            }
            _ => false,
        }
    }

    /// The earliest entry, as `(deadline, slot)`.
    pub fn peek(&self) -> Option<(u64, usize)> {
        (self.len > 0).then(|| {
            let slot = self.heap[0] as usize;
            (self.key[slot], slot)
        })
    }

    /// Remove and return the earliest entry if it is due at `now`.
    pub fn pop_due(&mut self, now: u64) -> Option<(u64, usize)> {
        let (deadline, slot) = self.peek()?;
        if deadline > now {
            return None;
        }
        self.remove_at(0);
        Some((deadline, slot))
    }

    /// Whether heap entry `a` sorts before `b`: earlier deadline, then lower
    /// slot, so equal deadlines expire in a fixed order.
    fn before(&self, a: usize, b: usize) -> bool {
        let (sa, sb) = (self.heap[a] as usize, self.heap[b] as usize);
        (self.key[sa], sa) < (self.key[sb], sb)
    }

    fn swap(&mut self, a: usize, b: usize) {
        self.heap.swap(a, b);
        self.pos[self.heap[a] as usize] = a as u16;
        self.pos[self.heap[b] as usize] = b as u16;
    }

    fn sift_up(&mut self, mut at: usize) -> usize {
        while at > 0 {
            let parent = (at - 1) / 2;
            if !self.before(at, parent) {
                break;
            }
            self.swap(at, parent);
            at = parent;
        }
        at
    }

    fn sift_down(&mut self, mut at: usize) {
        loop {
            let (left, right) = (2 * at + 1, 2 * at + 2);
            let mut best = at;
            if left < self.len && self.before(left, best) {
                best = left;
            }
            if right < self.len && self.before(right, best) {
                best = right;
            }
            if best == at {
                return;
            }
            self.swap(at, best);
            at = best;
        }
    }

    fn remove_at(&mut self, at: usize) {
        let slot = self.heap[at] as usize;
        let last = self.len - 1;
        self.swap(at, last);
        self.len = last;
        self.pos[slot] = NONE;
        if at < self.len {
            let at = self.sift_up(at);
            self.sift_down(at);
        }
    }

    /// Check the heap property and the slot index (test hook): every entry
    /// is no earlier than its parent and `pos` points back at it.
    #[allow(dead_code)]
    pub fn check(&self) -> Result<(), &'static str> {
        for at in 0..self.len {
            if self.pos[self.heap[at] as usize] as usize != at {
                return Err("pos does not point back at its heap entry");
            }
            if at > 0 && self.before(at, (at - 1) / 2) {
                return Err("an entry sorts before its parent");
            }
        }
        let queued = self.pos.iter().filter(|&&p| p != NONE).count();
        if queued != self.len {
            return Err("pos and len disagree");
        }
        Ok(())
    }
}
