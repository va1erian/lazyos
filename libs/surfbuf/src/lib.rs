//! Buffer-slot state machines for the display v1 pipelined present (#361).
//!
//! Two pure, allocation-free halves of one protocol, kept free of Messenger
//! and syscalls so the kernel test suite and host tests can drive them:
//!
//! * [`SlotTable`] is the compositor's view of one surface: which buffer
//!   slots are attached and which one is *current* (the only one the
//!   compositor reads). It decides what a `Present`/`AttachBufferSlot` may do
//!   and which slot to report in `BufferRelease`.
//! * [`Swapchain`] is the client's view: which slots it may write, and the
//!   in-order `FrameDone` accounting.
//!
//! The safety property both enforce: a client only writes slots it holds as
//! free, and the compositor only reads the current slot, so the two sets are
//! disjoint and a frame can never tear.
#![no_std]

/// Buffer slots per surface (`AttachBufferSlot` slot ids are `0..MAX_SLOTS`).
pub const MAX_SLOTS: usize = 4;

/// Damage rectangles per `Present`; more than this means "whole surface".
pub const MAX_DAMAGE: usize = 16;

/// Why a [`SlotTable::attach`] was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AttachError {
    /// The slot id is `>= MAX_SLOTS`.
    BadSlot,
    /// The slot is the current one, which the compositor may be reading.
    Busy,
}

/// Why a [`SlotTable::present`] was refused. The state is unchanged.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PresentError {
    /// The slot id is `>= MAX_SLOTS` or nothing is attached there.
    BadSlot,
}

/// The compositor-side slot table of one surface; `T` is whatever the
/// compositor keeps per mapped buffer (an address and length).
#[derive(Debug)]
pub struct SlotTable<T> {
    slots: [Option<T>; MAX_SLOTS],
    current: Option<usize>,
    /// Set by the first `Present`: only such surfaces get release events, so
    /// a legacy `AttachBuffer`/`Commit` client sees no new traffic.
    pipelined: bool,
}

impl<T> Default for SlotTable<T> {
    fn default() -> Self {
        SlotTable {
            slots: [const { None }; MAX_SLOTS],
            current: None,
            pipelined: false,
        }
    }
}

/// A validated slot index, or `None` when `slot` is out of range.
fn index_of(slot: u32) -> Option<usize> {
    usize::try_from(slot).ok().filter(|&i| i < MAX_SLOTS)
}

impl<T> SlotTable<T> {
    /// An empty table: nothing attached, nothing current.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `value` in `slot`. Returns the payload it replaced, which the
    /// caller must unmap. The current slot cannot be replaced.
    pub fn attach(&mut self, slot: u32, value: T) -> Result<Option<T>, AttachError> {
        let index = index_of(slot).ok_or(AttachError::BadSlot)?;
        if self.current == Some(index) {
            return Err(AttachError::Busy);
        }
        Ok(self.slots[index].replace(value))
    }

    /// Empty `slot` (`DetachBufferSlot`). Returns the payload it held, which
    /// the caller must unmap, or `None` for an empty slot. The current slot
    /// cannot be detached.
    pub fn detach(&mut self, slot: u32) -> Result<Option<T>, AttachError> {
        let index = index_of(slot).ok_or(AttachError::BadSlot)?;
        if self.current == Some(index) {
            return Err(AttachError::Busy);
        }
        Ok(self.slots[index].take())
    }

    /// The legacy `AttachBuffer`: `value` replaces slot 0 and becomes current
    /// at once, even if the compositor was reading the old one (the tear-prone
    /// behaviour the slot API exists to avoid). Returns the payload to unmap.
    ///
    /// Refused with [`AttachError::Busy`] once the surface has used
    /// `Present`: the client then owns slots by `BufferRelease`, and taking
    /// slot 0 back behind its swapchain would strand or tear a slot.
    pub fn attach_legacy(&mut self, value: T) -> Result<Option<T>, AttachError> {
        if self.pipelined {
            return Err(AttachError::Busy);
        }
        self.current = Some(0);
        Ok(self.slots[0].replace(value))
    }

    /// Make `slot` current. On success returns the slot the compositor just
    /// stopped reading (to report in `BufferRelease`), or `None` when the
    /// current slot did not change or there was no previous one.
    pub fn present(&mut self, slot: u32) -> Result<Option<u32>, PresentError> {
        let index = index_of(slot)
            .filter(|&i| self.slots[i].is_some())
            .ok_or(PresentError::BadSlot)?;
        self.pipelined = true;
        let old = self.current.replace(index);
        Ok(old.filter(|&o| o != index).map(|o| o as u32))
    }

    /// The buffer the compositor reads, if any.
    pub fn current(&self) -> Option<&T> {
        self.current.and_then(|i| self.slots[i].as_ref())
    }

    /// The current slot id.
    pub fn current_slot(&self) -> Option<u32> {
        self.current.map(|i| i as u32)
    }

    /// Whether this surface has used `Present` (and so gets events).
    pub fn is_pipelined(&self) -> bool {
        self.pipelined
    }

    /// Detach everything (surface destroyed); the caller unmaps each payload.
    pub fn take_all(&mut self) -> [Option<T>; MAX_SLOTS] {
        self.current = None;
        core::mem::take(&mut self.slots)
    }
}

/// A rectangle in surface-content pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Area {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Clip a `Present` damage list to a `width` x `height` surface.
///
/// Returns the rectangles to repaint and how many are valid. An empty list,
/// or one longer than [`MAX_DAMAGE`], means the whole surface; zero-area and
/// fully outside rectangles are dropped, and coordinates use `u64` sums so a
/// hostile `x + w` cannot wrap. A non-empty list that clips to nothing yields
/// zero rectangles (nothing to repaint).
pub fn clip_damage<I>(width: u32, height: u32, rects: I) -> ([Area; MAX_DAMAGE], usize)
where
    I: ExactSizeIterator<Item = Area>,
{
    let mut out = [Area::default(); MAX_DAMAGE];
    let whole = Area {
        x: 0,
        y: 0,
        w: width,
        h: height,
    };
    if rects.len() == 0 || rects.len() > MAX_DAMAGE {
        out[0] = whole;
        return (out, usize::from(width > 0 && height > 0));
    }
    let mut count = 0;
    for rect in rects {
        let x0 = u64::from(rect.x.min(width));
        let y0 = u64::from(rect.y.min(height));
        let x1 = (u64::from(rect.x) + u64::from(rect.w)).min(u64::from(width));
        let y1 = (u64::from(rect.y) + u64::from(rect.h)).min(u64::from(height));
        if x1 > x0 && y1 > y0 {
            out[count] = Area {
                x: x0 as u32,
                y: y0 as u32,
                w: (x1 - x0) as u32,
                h: (y1 - y0) as u32,
            };
            count += 1;
        }
    }
    (out, count)
}

/// What the client knows about one slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Owner {
    /// The client may write it.
    Client,
    /// Submitted; the compositor may read it until `BufferRelease`.
    Compositor,
}

/// The client-side swapchain over `count` slots.
#[derive(Clone, Copy, Debug)]
pub struct Swapchain {
    count: usize,
    owner: [Owner; MAX_SLOTS],
    /// Seq of the most recent present (`0` before any).
    submitted: u64,
    /// Seq of the most recent completed frame.
    done: u64,
}

impl Swapchain {
    /// A swapchain of `count` slots (clamped to `2..=MAX_SLOTS`), all free.
    /// One slot is not enough: the compositor releases a slot only when a
    /// different one replaces it, so a single-slot chain would never get its
    /// buffer back after the first present.
    pub fn new(count: usize) -> Swapchain {
        Swapchain {
            count: count.clamp(2, MAX_SLOTS),
            owner: [Owner::Client; MAX_SLOTS],
            submitted: 0,
            done: 0,
        }
    }

    /// Slots in this chain.
    pub fn count(&self) -> usize {
        self.count
    }

    /// A slot the client may draw into now, or `None` until a
    /// `BufferRelease` frees one. Repeated calls return the same slot until
    /// it is submitted.
    pub fn acquire(&self) -> Option<u32> {
        (0..self.count)
            .find(|&i| self.owner[i] == Owner::Client)
            .map(|i| i as u32)
    }

    /// Record that `slot` was submitted with `Present`; returns the sequence
    /// number to send (starting at 1). `None` if the slot is not free.
    pub fn submit(&mut self, slot: u32) -> Option<u64> {
        let index = index_of(slot).filter(|&i| i < self.count)?;
        if self.owner[index] != Owner::Client {
            return None;
        }
        self.owner[index] = Owner::Compositor;
        self.submitted += 1;
        Some(self.submitted)
    }

    /// Fold in a `BufferRelease`; `false` for a slot we did not hold.
    pub fn released(&mut self, slot: u32) -> bool {
        match index_of(slot).filter(|&i| i < self.count) {
            Some(index) if self.owner[index] == Owner::Compositor => {
                self.owner[index] = Owner::Client;
                true
            }
            _ => false,
        }
    }

    /// Undo the most recent [`Swapchain::submit`] (`slot`, numbered `seq`)
    /// when its `Present` never left the client: the compositor will neither
    /// release the slot nor finish the frame. `false` (nothing changes) for
    /// anything but the latest submit.
    pub fn cancel(&mut self, slot: u32, seq: u64) -> bool {
        let Some(index) = index_of(slot).filter(|&i| i < self.count) else {
            return false;
        };
        if seq != self.submitted || seq <= self.done || self.owner[index] != Owner::Compositor {
            return false;
        }
        self.owner[index] = Owner::Client;
        self.submitted -= 1;
        true
    }

    /// Fold in a `FrameDone`; frames complete in order, so anything but the
    /// next outstanding seq is refused (`false`).
    pub fn frame_done(&mut self, seq: u64) -> bool {
        if seq != self.done + 1 || seq > self.submitted {
            return false;
        }
        self.done = seq;
        true
    }

    /// Presents submitted and not yet completed.
    pub fn in_flight(&self) -> u64 {
        self.submitted - self.done
    }

    /// Seq of the last completed frame.
    pub fn completed(&self) -> u64 {
        self.done
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    #[test]
    fn attach_rules() {
        let mut table = SlotTable::new();
        assert_eq!(table.attach(0, 10), Ok(None));
        assert_eq!(table.attach(4, 1), Err(AttachError::BadSlot));
        assert_eq!(table.attach(u32::MAX, 1), Err(AttachError::BadSlot));
        assert_eq!(table.present(0), Ok(None));
        assert_eq!(table.attach(0, 11), Err(AttachError::Busy));
        assert_eq!(table.attach(1, 12), Ok(None));
        assert_eq!(table.attach(1, 13), Ok(Some(12)));
    }

    #[test]
    fn detach_rules() {
        let mut table = SlotTable::new();
        assert_eq!(table.detach(4), Err(AttachError::BadSlot));
        assert_eq!(table.detach(1), Ok(None), "an empty slot");
        table.attach(0, 10).unwrap();
        table.attach(1, 11).unwrap();
        table.present(0).unwrap();
        assert_eq!(table.detach(0), Err(AttachError::Busy));
        assert_eq!(table.current(), Some(&10));
        assert_eq!(table.detach(1), Ok(Some(11)));
        assert_eq!(table.present(1), Err(PresentError::BadSlot), "now empty");
        assert_eq!(table.attach(1, 12), Ok(None), "reattachable");
    }

    #[test]
    fn present_reports_the_replaced_slot() {
        let mut table = SlotTable::new();
        assert_eq!(table.present(0), Err(PresentError::BadSlot));
        table.attach(0, 1).unwrap();
        table.attach(1, 2).unwrap();
        assert!(!table.is_pipelined());
        assert_eq!(table.present(0), Ok(None));
        assert!(table.is_pipelined());
        assert_eq!(table.present(0), Ok(None));
        assert_eq!(table.present(1), Ok(Some(0)));
        assert_eq!(table.current(), Some(&2));
        assert_eq!(table.present(3), Err(PresentError::BadSlot));
        assert_eq!(table.current_slot(), Some(1));
    }

    #[test]
    fn legacy_attach_replaces_slot_zero_and_is_current() {
        let mut table = SlotTable::new();
        assert_eq!(table.attach_legacy(1), Ok(None));
        assert_eq!(table.attach_legacy(2), Ok(Some(1)));
        assert_eq!(table.current(), Some(&2));
        assert!(!table.is_pipelined());
    }

    #[test]
    fn legacy_attach_is_refused_once_pipelined() {
        let mut table = SlotTable::new();
        table.attach(1, 5).unwrap();
        table.present(1).unwrap();
        assert_eq!(table.attach_legacy(9), Err(AttachError::Busy));
        assert_eq!(table.current_slot(), Some(1));
        assert_eq!(table.current(), Some(&5));
    }

    #[test]
    fn take_all_returns_every_payload() {
        let mut table = SlotTable::new();
        table.attach(0, 1).unwrap();
        table.attach(2, 3).unwrap();
        table.present(2).unwrap();
        let all = table.take_all();
        assert_eq!(all.iter().flatten().count(), 2);
        assert_eq!(table.current(), None);
    }

    fn clip(width: u32, height: u32, rects: &[Area]) -> Vec<Area> {
        let (out, count) = clip_damage(width, height, rects.iter().copied());
        out[..count].to_vec()
    }

    const fn area(x: u32, y: u32, w: u32, h: u32) -> Area {
        Area { x, y, w, h }
    }

    #[test]
    fn empty_damage_is_the_whole_surface() {
        assert_eq!(clip(30, 20, &[]), [area(0, 0, 30, 20)]);
        assert!(clip(0, 20, &[]).is_empty());
    }

    #[test]
    fn oversized_damage_list_is_the_whole_surface() {
        let many = [area(1, 1, 2, 2); MAX_DAMAGE + 1];
        assert_eq!(clip(30, 20, &many), [area(0, 0, 30, 20)]);
        let exact = [area(1, 1, 2, 2); MAX_DAMAGE];
        assert_eq!(clip(30, 20, &exact).len(), MAX_DAMAGE);
    }

    #[test]
    fn damage_is_clipped_and_degenerate_rects_dropped() {
        let rects = [
            area(5, 5, 10, 10),
            area(25, 15, 100, 100),
            area(0, 0, 0, 5),
            area(40, 0, 5, 5),
            area(u32::MAX, u32::MAX, u32::MAX, u32::MAX),
            area(2, 2, u32::MAX, u32::MAX),
        ];
        assert_eq!(
            clip(30, 20, &rects),
            [area(5, 5, 10, 10), area(25, 15, 5, 5), area(2, 2, 28, 18)]
        );
        assert!(clip(30, 20, &[area(30, 0, 5, 5)]).is_empty());
    }

    #[test]
    fn swapchain_double_buffer_flow() {
        let mut chain = Swapchain::new(2);
        let a = chain.acquire().unwrap();
        assert_eq!(chain.submit(a), Some(1));
        let b = chain.acquire().unwrap();
        assert_ne!(a, b);
        assert_eq!(chain.submit(b), Some(2));
        assert_eq!(chain.acquire(), None);
        assert!(chain.released(a));
        assert!(!chain.released(a));
        assert_eq!(chain.acquire(), Some(a));
        assert!(!chain.frame_done(2));
        assert!(chain.frame_done(1));
        assert!(chain.frame_done(2));
        assert!(!chain.frame_done(3));
        assert_eq!(chain.in_flight(), 0);
    }

    #[test]
    fn a_cancelled_submit_frees_the_slot_and_its_seq() {
        let mut chain = Swapchain::new(2);
        assert_eq!(chain.submit(0), Some(1));
        assert_eq!(chain.submit(1), Some(2));
        assert!(!chain.cancel(0, 1), "only the latest submit can be undone");
        assert!(!chain.cancel(1, 3));
        assert!(chain.cancel(1, 2));
        assert_eq!(chain.in_flight(), 1);
        assert_eq!(chain.acquire(), Some(1));
        assert_eq!(chain.submit(1), Some(2), "the seq is reused");
        assert!(chain.frame_done(1));
        assert!(chain.frame_done(2));
        assert!(!chain.cancel(1, 2), "a finished frame cannot be undone");
    }

    #[test]
    fn swapchain_refuses_bad_slots() {
        let mut chain = Swapchain::new(9);
        assert_eq!(chain.count(), MAX_SLOTS);
        assert_eq!(chain.submit(4), None);
        chain.submit(0).unwrap();
        assert_eq!(chain.submit(0), None);
        assert!(!chain.released(7));
        assert_eq!(Swapchain::new(0).count(), 2);
        assert_eq!(Swapchain::new(1).count(), 2);
    }
}
