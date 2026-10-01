//! The double-buffered pixel slots behind one client window (issue #372).
//!
//! The compositor reads the slot presented last while the app draws into the
//! other, so a frame never tears and a present never waits for a reply. Each
//! slot remembers which of its pixels are older than the backend's painting
//! surface (`stale`): a frame's damage goes stale in *every* slot, and filling
//! a slot copies only its own stale rows, so slot A catches up on the frame
//! it missed while slot B was on screen (issue #487).
//!
//! A slot is (re)allocated when it is next drawn into at a size other than
//! its own, which is how a `Configure` reaches the buffers: the free slot is
//! never the one the compositor reads, so attaching to it is always allowed.

use surfbuf::Swapchain;
use xui_core::Rect;

use crate::display::Client;
use crate::sys::{self, errno};

/// Slots per window: one on screen, one being drawn.
pub const SLOTS: usize = 2;

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
    /// The bounding box of the pixels older than the painting surface, or
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
        }
    }

    /// A slot the app may fill now, holding a `width` x `height` buffer
    /// (attaching a new one when the window was resized since the slot was
    /// last used). `Ok(None)` while the compositor holds every slot: the next
    /// `BufferRelease` frees one. An error (no quota, or `EINVAL` when a newer
    /// `Configure` is on its way) leaves the slot bufferless and free, so a
    /// later frame simply tries again.
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
            if entry.buffer != 0 {
                let _ = sys::display_close_buffer(entry.buffer);
            }
            *entry = Slot::default();
            let (buffer, va) = attach_slot(client, surface, slot, width, height)?;
            *entry = Slot {
                buffer,
                va,
                width,
                height,
                stale: Some(Rect::new(0, 0, width, height)),
            };
        }
        Ok(Some(slot))
    }

    /// Record that the painting surface changed inside `rect`: every slot is
    /// now stale there.
    pub fn damage(&mut self, rect: Rect) {
        for slot in &mut self.slots {
            slot.stale = Some(slot.stale.map_or(rect, |stale| union(stale, rect)));
        }
    }

    /// Bring `slot` in sync with `pixels`, the window's top-down RGBA image of
    /// the slot's own size, by copying only its stale rows. `false` when
    /// `pixels` has the wrong length (a resize the slot has not caught up
    /// with), leaving the slot untouched.
    pub fn sync(&mut self, slot: u32, pixels: &[u8]) -> bool {
        let Some(entry) = self.slots.get_mut(slot as usize) else {
            return false;
        };
        let (width, height) = (entry.width, entry.height);
        if entry.buffer == 0 || pixels.len() != width as usize * height as usize * 4 {
            return false;
        }
        if let Some(stale) = entry.stale.take() {
            // SAFETY: `va` maps the buffer this slot created for exactly
            // `pixels.len()` (`width * height * 4`) bytes, which stays mapped
            // until the slot is closed; the slot is free, so the compositor is
            // not reading it, and nothing else in this task aliases it.
            let buffer =
                unsafe { core::slice::from_raw_parts_mut(entry.va as *mut u8, pixels.len()) };
            copy_rect(buffer, pixels, width, height, stale);
        }
        true
    }

    /// Hand `slot` to the compositor; the `Present` sequence number to send,
    /// or `None` when the slot was not free.
    pub fn submit(&mut self, slot: u32) -> Option<u64> {
        let seq = self.chain.submit(slot)?;
        self.last = Some(slot);
        Some(seq)
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
            let _ = sys::display_close_buffer(slot.buffer);
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
    let (buffer, va, _) = sys::display_create_buffer(size)?;
    if let Err(code) = client.attach_slot(surface, slot, buffer, size) {
        let _ = sys::display_close_buffer(buffer);
        return Err(code);
    }
    Ok((buffer, va))
}

/// The smallest rectangle covering `a` and `b`.
fn union(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.left.min(b.left),
        a.top.min(b.top),
        a.right.max(b.right),
        a.bottom.max(b.bottom),
    )
}

/// Copy `rect` (clipped to the image) of the `width` x `height` RGBA image
/// `src` into the same place in `dst`: one copy for full-width rows, else one
/// per row. Nothing is copied unless both slices are exactly that long.
pub fn copy_rect(dst: &mut [u8], src: &[u8], width: i32, height: i32, rect: Rect) {
    let stride = width.max(0) as usize * 4;
    let len = stride * height.max(0) as usize;
    if dst.len() != len || src.len() != len {
        return;
    }
    let left = rect.left.clamp(0, width) as usize;
    let right = rect.right.clamp(0, width) as usize;
    let top = rect.top.clamp(0, height) as usize;
    let bottom = rect.bottom.clamp(0, height) as usize;
    if left >= right || top >= bottom {
        return;
    }
    if right - left == width as usize {
        let span = top * stride..bottom * stride;
        dst[span.clone()].copy_from_slice(&src[span]);
        return;
    }
    for row in top..bottom {
        let span = row * stride + left * 4..row * stride + right * 4;
        dst[span.clone()].copy_from_slice(&src[span]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: i32 = 8;
    const H: i32 = 6;
    const BYTES: usize = (W * H * 4) as usize;

    /// An image whose every byte is `value`.
    fn image(value: u8) -> Vec<u8> {
        vec![value; BYTES]
    }

    /// The bytes of pixel `(x, y)` in `buffer`.
    fn pixel(buffer: &[u8], x: i32, y: i32) -> &[u8] {
        let at = ((y * W + x) * 4) as usize;
        &buffer[at..at + 4]
    }

    #[test]
    fn a_fresh_slot_takes_the_whole_image() {
        let (mut a, mut b) = (image(0), image(0));
        let mut slots = Slots::for_tests([&mut a, &mut b], W, H);
        assert!(slots.sync(0, &image(7)));
        assert!(!slots.is_stale(0));
        assert!(a.iter().all(|&byte| byte == 7));
    }

    #[test]
    fn only_the_stale_rows_are_copied() {
        let (mut a, mut b) = (image(0), image(0));
        let mut slots = Slots::for_tests([&mut a, &mut b], W, H);
        slots.sync(0, &image(1));
        slots.sync(1, &image(1));
        // The surface changes inside (2,1)-(5,3) only.
        slots.damage(Rect::new(2, 1, 5, 3));
        assert!(slots.sync(0, &image(9)));
        assert_eq!(pixel(&a, 2, 1), [9; 4]);
        assert_eq!(pixel(&a, 4, 2), [9; 4]);
        assert_eq!(pixel(&a, 5, 2), [1; 4], "right of the damage");
        assert_eq!(pixel(&a, 1, 1), [1; 4], "left of the damage");
        assert_eq!(pixel(&a, 2, 3), [1; 4], "below the damage");
        assert_eq!(pixel(&a, 2, 0), [1; 4], "above the damage");
    }

    #[test]
    fn a_slot_catches_up_on_the_frame_it_missed() {
        let (mut a, mut b) = (image(0), image(0));
        let mut slots = Slots::for_tests([&mut a, &mut b], W, H);
        slots.sync(0, &image(1));
        slots.sync(1, &image(1));
        // Frame 1 damages the top row and lands in slot 0.
        let mut surface = image(1);
        surface[..(W * 4) as usize].fill(2);
        slots.damage(Rect::new(0, 0, W, 1));
        slots.sync(0, &surface);
        // Frame 2 damages the bottom row and lands in slot 1, which must also
        // pick up frame 1's top row.
        let last = ((H - 1) * W * 4) as usize;
        surface[last..].fill(3);
        slots.damage(Rect::new(0, H - 1, W, H));
        slots.sync(1, &surface);
        assert_eq!(b, surface);
        assert!(slots.is_stale(0), "slot 0 still lacks frame 2");
    }

    #[test]
    fn damage_outside_the_image_is_clipped() {
        let (mut a, mut b) = (image(0), image(0));
        let mut slots = Slots::for_tests([&mut a, &mut b], W, H);
        slots.sync(0, &image(1));
        slots.damage(Rect::new(-10, -10, 100, 100));
        assert!(slots.sync(0, &image(4)));
        assert!(a.iter().all(|&byte| byte == 4));
    }

    #[test]
    fn a_wrong_size_image_is_refused() {
        let (mut a, mut b) = (image(0), image(0));
        let mut slots = Slots::for_tests([&mut a, &mut b], W, H);
        assert!(!slots.sync(0, &[5; 16]));
        assert!(slots.is_stale(0), "a refused sync keeps the slot stale");
        assert!(a.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn releasing_the_newest_slot_means_the_present_was_refused() {
        let (mut a, mut b) = (image(0), image(0));
        let mut slots = Slots::for_tests([&mut a, &mut b], W, H);
        assert_eq!(slots.submit(0), Some(1));
        assert_eq!(slots.submit(1), Some(2));
        assert!(
            !slots.released(0),
            "slot 1 replaced slot 0: a normal release"
        );
        assert!(slots.released(1), "nothing replaced slot 1: refused");
        assert!(!slots.released(1), "a slot is only released once");
    }
}
