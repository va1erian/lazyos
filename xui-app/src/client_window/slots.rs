//! The double-buffered pixel slots behind one client window (issue #372).
//!
//! The compositor reads the slot presented last while the app draws into the
//! other, so a frame never tears and a present never waits for a reply. Each
//! slot remembers which of its pixels are older than the window's picture
//! (`stale`): a frame's damage goes stale in every *other* slot (issue #487).
//! A frame is composed straight into the slot it fills (buffer age): what the
//! slot missed outside the new damage is first copied from another slot that
//! is current there (with two slots, the one composed last), so a frame that
//! repaints a whole page view writes each of its pixels once instead of into
//! a private frame and then the slot.
//!
//! A slot is (re)allocated when it is next drawn into at a size other than
//! its own, which is how a `Configure` reaches the buffers: the free slot is
//! never the one the compositor reads, so attaching to it is always allowed.

use surfbuf::Swapchain;
use xui_core::Rect;

use super::rects::{copy_rect, intersection, subtract, union};
use crate::display::Client;
use crate::sys::{self, errno};

/// Slots per window: one on screen, one being drawn.
pub const SLOTS: usize = 2;

/// PIT ticks (100 Hz) before a buffer allocation that failed at a size is
/// tried again at that same size. Out of quota, every frame would otherwise
/// create and close a buffer; a new size (a newer `Configure`) retries at once.
const RETRY_TICKS: u64 = 50;

/// One shared pixel buffer attached as a slot.
#[derive(Clone, Copy, Default)]
struct Slot {
    /// The buffer handle; `0` while the slot has none.
    buffer: u64,
    /// The buffer's mapping in this task.
    va: u64,
    /// The size the buffer was created for, in pixels.
    width: i32,
    height: i32,
    /// The bounding box of the pixels older than the window's picture, or
    /// `None` when the slot is in sync with it.
    stale: Option<Rect>,
}

impl Slot {
    /// Whether this slot holds a buffer of exactly `width` x `height`: the
    /// compositor reads a slot with the stride it was attached at, so a
    /// buffer that is merely large enough would shear the image.
    fn fits(&self, width: i32, height: i32) -> bool {
        self.buffer != 0 && (self.width, self.height) == (width, height)
    }
}

/// A window's slots and the swapchain that says which one the app may write.
pub struct Slots {
    slots: [Slot; SLOTS],
    chain: Swapchain,
    /// The slot submitted last. The compositor only releases a slot when a
    /// newer present replaces it, so a release of this one means it refused
    /// the present and never showed the frame.
    last: Option<u32>,
    /// `last` before the latest submit, restored when that submit is
    /// cancelled.
    before: Option<u32>,
    /// The size an allocation last failed at, and the tick before which
    /// [`Slots::acquire`] does not try that size again.
    failed: Option<((i32, i32), u64)>,
}

impl Slots {
    /// Slots for a window whose slot 0 is already attached (`buffer` mapped at
    /// `va`, `width` x `height`); slot 1 is allocated on first use.
    pub fn new(buffer: u64, va: u64, width: i32, height: i32) -> Slots {
        let mut slots = [Slot::default(); SLOTS];
        slots[0] = Slot {
            buffer,
            va,
            width,
            height,
            stale: Some(Rect::new(0, 0, width, height)),
        };
        Slots {
            slots,
            chain: Swapchain::new(SLOTS),
            last: None,
            before: None,
            failed: None,
        }
    }

    /// Give every bufferless slot a buffer of slot 0's size, so a window
    /// that cannot be double-buffered (no buffer quota) fails to open instead
    /// of freezing after its first frame.
    pub fn reserve(&mut self, client: Client, surface: u64) -> Result<(), i64> {
        let (width, height) = (self.slots[0].width, self.slots[0].height);
        for (slot, entry) in self.slots.iter_mut().enumerate() {
            if entry.buffer == 0 {
                let (buffer, va) = attach_slot(client, surface, slot as u32, width, height)?;
                *entry = Slot {
                    buffer,
                    va,
                    width,
                    height,
                    stale: Some(Rect::new(0, 0, width, height)),
                };
            }
        }
        Ok(())
    }

    /// A slot the app may fill now, holding a `width` x `height` buffer
    /// (attaching a new one when the window was resized since the slot was
    /// last used). `Ok(None)` while the compositor holds every slot: the next
    /// `BufferRelease` frees one. An error (no quota, or `EINVAL` when a newer
    /// `Configure` is on its way) leaves the slot bufferless and free, so a
    /// later frame tries again: at once for a new size, after
    /// [`RETRY_TICKS`] for the same one (`EAGAIN` until then).
    pub fn acquire(
        &mut self,
        client: Client,
        surface: u64,
        width: i32,
        height: i32,
    ) -> Result<Option<u32>, i64> {
        let Some(slot) = self.chain.acquire() else {
            return Ok(None);
        };
        let entry = &mut self.slots[slot as usize];
        if !entry.fits(width, height) {
            // A free slot is not the compositor's current one, so its old
            // buffer can go first: freeing it before allocating keeps a
            // resize inside the per-process buffer quota.
            let now = sys::clock_ticks();
            if matches!(self.failed, Some((size, until)) if size == (width, height) && now < until)
            {
                return Err(-errno::EAGAIN);
            }
            if entry.buffer != 0 {
                let _ = sys::buffer_close(entry.buffer);
            }
            *entry = Slot::default();
            let attached = attach_slot(client, surface, slot, width, height);
            let Ok((buffer, va)) = attached else {
                self.failed = Some(((width, height), now.saturating_add(RETRY_TICKS)));
                return attached.map(|_| None);
            };
            self.failed = None;
            self.slots[slot as usize] = Slot {
                buffer,
                va,
                width,
                height,
                stale: Some(Rect::new(0, 0, width, height)),
            };
        }
        Ok(Some(slot))
    }

    /// Make `slot` ready for a frame whose `damage` is composed straight into
    /// it (buffer age): the part of the slot's stale pixels the damage does
    /// not cover is copied from another slot that holds it (the one composed
    /// last, in steady state), and every other slot goes stale where this
    /// frame composes. Returns what to compose: `damage` clipped to the slot,
    /// or more of the slot when no other slot can supply the rest (the first
    /// frames, a resize). `None` for a bufferless slot or nothing to compose.
    ///
    /// The slot is in sync once the returned rectangle is composed into it;
    /// a frame that fails before that must [`invalidate`](Self::invalidate)
    /// it.
    pub fn prepare(&mut self, slot: u32, damage: Rect) -> Option<Rect> {
        let index = slot as usize;
        let entry = *self.slots.get(index)?;
        if entry.buffer == 0 {
            return None;
        }
        let full = Rect::new(0, 0, entry.width, entry.height);
        let mut compose = intersection(damage, full)?;
        if let Some(stale) = entry.stale.and_then(|stale| intersection(stale, full)) {
            let missing = subtract(stale, compose);
            if !missing.is_empty() {
                match self.source(index, &missing) {
                    Some(source) => self.copy_from(source, index, &missing),
                    None => compose = union(compose, stale),
                }
            }
        }
        for (other, entry) in self.slots.iter_mut().enumerate() {
            entry.stale = (other != index)
                .then(|| entry.stale.map_or(compose, |stale| union(stale, compose)));
        }
        Some(compose)
    }

    /// A slot other than `index`, of its size, whose pixels are current
    /// everywhere in `missing`.
    fn source(&self, index: usize, missing: &[Rect]) -> Option<usize> {
        let (width, height) = (self.slots[index].width, self.slots[index].height);
        let current = |stale: Option<Rect>| {
            stale.is_none_or(|stale| missing.iter().all(|r| intersection(*r, stale).is_none()))
        };
        (0..SLOTS).find(|&other| {
            let slot = &self.slots[other];
            other != index && slot.fits(width, height) && current(slot.stale)
        })
    }

    /// Copy `rects` of slot `from` into slot `to`, both of one size.
    fn copy_from(&mut self, from: usize, to: usize, rects: &[Rect]) {
        let (src, dst) = (self.slots[from], self.slots[to]);
        let len = dst.width as usize * dst.height as usize * 4;
        // SAFETY: each `va` maps the buffer its slot created for exactly
        // `width * height * 4` bytes (the two slots fit one size), mapped
        // until the slot is closed. They are two distinct buffers, so the
        // slices do not overlap; `to` is free (the compositor is not reading
        // it) and nothing else in this task aliases it, and `from` is only
        // read, which the compositor's own reads of it do not disturb.
        let (src, dst) = unsafe {
            (
                core::slice::from_raw_parts(src.va as *const u8, len),
                core::slice::from_raw_parts_mut(dst.va as *mut u8, len),
            )
        };
        for rect in rects {
            copy_rect(dst, src, self.slots[to].width, self.slots[to].height, *rect);
        }
    }

    /// Mark all of `slot` stale: a frame composed into it failed part way.
    pub fn invalidate(&mut self, slot: u32) {
        if let Some(entry) = self.slots.get_mut(slot as usize) {
            entry.stale = Some(Rect::new(0, 0, entry.width, entry.height));
        }
    }

    /// The mapping of `slot` (address and length in bytes) when it holds a
    /// `width` x `height` buffer. Writing through it is only sound while the
    /// slot is free and stays open: between [`acquire`](Self::acquire) and
    /// [`submit`](Self::submit), with the window kept from closing.
    pub fn mapping(&self, slot: u32, width: i32, height: i32) -> Option<(usize, usize)> {
        let entry = self.slots.get(slot as usize)?;
        let len = width as usize * height as usize * 4;
        entry
            .fits(width, height)
            .then_some((entry.va as usize, len))
    }

    /// Copy `rect` of `pixels`, a `width` x `height` RGBA image, into `slot`;
    /// `false` (nothing copied) when the slot is not of that size.
    pub fn write(&mut self, slot: u32, pixels: &[u8], width: i32, height: i32, rect: Rect) -> bool {
        let Some((va, len)) = self.mapping(slot, width, height) else {
            return false;
        };
        if pixels.len() != len {
            return false;
        }
        // SAFETY: `mapping` checked the slot holds a buffer of exactly `len`
        // bytes, mapped at `va` until the slot is closed; the caller writes
        // only a free slot, which the compositor does not read, and nothing
        // else in this task aliases it while `self` is borrowed mutably.
        let buffer = unsafe { core::slice::from_raw_parts_mut(va as *mut u8, len) };
        copy_rect(buffer, pixels, width, height, rect);
        true
    }

    /// Hand `slot` to the compositor; the `Present` sequence number to send,
    /// or `None` when the slot was not free.
    pub fn submit(&mut self, slot: u32) -> Option<u64> {
        let seq = self.chain.submit(slot)?;
        self.before = self.last.replace(slot);
        Some(seq)
    }

    /// Take back `slot`, submitted as `seq`, whose `Present` never reached
    /// the compositor: no `BufferRelease` will free it. Its pixels stay in
    /// sync, so a retry only needs the damage presented again.
    pub fn cancel(&mut self, slot: u32, seq: u64) -> bool {
        if !self.chain.cancel(slot, seq) {
            return false;
        }
        self.last = self.before.take();
        true
    }

    /// Fold in a `BufferRelease`. Returns `true` when it refused the present
    /// of `slot`: the frame never reached the screen, so the caller must
    /// present it again.
    pub fn released(&mut self, slot: u32) -> bool {
        if !self.chain.released(slot) {
            return false;
        }
        let refused = self.last == Some(slot);
        if refused {
            self.last = None;
        }
        refused
    }

    /// Fold in a `FrameDone`; `false` for an out-of-order sequence number.
    pub fn frame_done(&mut self, seq: u64) -> bool {
        self.chain.frame_done(seq)
    }

    /// Release every buffer this window created. The compositor keeps its
    /// own reference to the attached ones until the surface is destroyed.
    pub fn close(&self) {
        for slot in self.slots.iter().filter(|slot| slot.buffer != 0) {
            let _ = sys::buffer_close(slot.buffer);
        }
    }

    /// Slots for host tests, backed by caller-owned memory instead of shared
    /// buffers.
    #[cfg(test)]
    pub(crate) fn for_tests(buffers: [&mut [u8]; SLOTS], width: i32, height: i32) -> Slots {
        let mut slots = Slots::new(buffers[0].as_mut_ptr() as u64, 0, width, height);
        slots.slots[0].va = slots.slots[0].buffer;
        slots.slots[1] = Slot {
            buffer: buffers[1].as_mut_ptr() as u64,
            ..slots.slots[0]
        };
        slots.slots[1].va = slots.slots[1].buffer;
        slots
    }

    /// Whether `slot` holds pixels older than the painting surface.
    #[cfg(test)]
    pub(crate) fn is_stale(&self, slot: u32) -> bool {
        self.slots[slot as usize].stale.is_some()
    }
}

/// Create a `width` x `height` buffer and attach it as `slot` of `surface`;
/// a refused attach closes the buffer again so it does not count against the
/// per-process quota.
fn attach_slot(
    client: Client,
    surface: u64,
    slot: u32,
    width: i32,
    height: i32,
) -> Result<(u64, u64), i64> {
    if width <= 0 || height <= 0 {
        return Err(-errno::EINVAL);
    }
    let size = width as u64 * height as u64 * 4;
    let (buffer, va, _) = sys::buffer_create(size)?;
    if let Err(code) = client.attach_slot(surface, slot, buffer, size) {
        let _ = sys::buffer_close(buffer);
        return Err(code);
    }
    Ok((buffer, va))
}

#[cfg(test)]
mod tests;
