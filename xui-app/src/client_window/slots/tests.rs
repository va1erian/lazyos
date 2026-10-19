//! Host tests of the slots: buffer age, catching up, refusals, presents.

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

/// One frame: prepare `slot` for `damage`, then compose what that asks
/// for from `picture`; the rectangle composed.
fn frame(slots: &mut Slots, slot: u32, damage: Rect, picture: &[u8]) -> Rect {
    let compose = slots.prepare(slot, damage).expect("a slot with a buffer");
    assert!(slots.write(slot, picture, W, H, compose));
    compose
}

/// `picture` with the rectangle `rect` set to `value`.
fn paint(picture: &mut [u8], rect: Rect, value: u8) {
    for y in rect.top..rect.bottom {
        for x in rect.left..rect.right {
            let at = ((y * W + x) * 4) as usize;
            picture[at..at + 4].fill(value);
        }
    }
}

#[test]
fn a_fresh_slot_is_composed_whole() {
    let (mut a, mut b) = (image(0), image(0));
    let mut slots = Slots::for_tests([&mut a, &mut b], W, H);
    let full = Rect::new(0, 0, W, H);
    // Nothing holds the picture yet, so a small damage grows to the slot.
    assert_eq!(slots.prepare(0, Rect::new(1, 1, 2, 2)), Some(full));
    assert!(!slots.is_stale(0));
    assert!(slots.is_stale(1));
    assert!(slots.write(0, &image(7), W, H, full));
    assert!(a.iter().all(|&byte| byte == 7));
}

#[test]
fn the_rest_of_a_slots_stale_part_comes_from_the_other_slot() {
    let (mut a, mut b) = (image(0), image(0));
    let mut slots = Slots::for_tests([&mut a, &mut b], W, H);
    let full = Rect::new(0, 0, W, H);
    let mut picture = image(1);
    frame(&mut slots, 0, full, &picture);
    frame(&mut slots, 1, full, &picture);
    // Frame 1 changes the top rows and lands in slot 0.
    paint(&mut picture, Rect::new(0, 0, W, 2), 2);
    assert_eq!(
        frame(&mut slots, 0, Rect::new(0, 0, W, 2), &picture),
        Rect::new(0, 0, W, 2)
    );
    // Frame 2 changes a box lower down and lands in slot 1, which takes
    // frame 1's rows from slot 0 and composes only its own damage.
    let damage = Rect::new(2, 3, 5, 5);
    paint(&mut picture, damage, 3);
    assert_eq!(frame(&mut slots, 1, damage, &picture), damage);
    assert!(!slots.is_stale(1));
    assert!(slots.is_stale(0), "slot 0 lacks frame 2");
    drop(slots);
    assert_eq!(b, picture);
    assert_eq!(pixel(&a, 3, 4), [1; 4], "slot 0 was never written frame 2");
}

#[test]
fn damage_covering_the_stale_part_copies_nothing() {
    let (mut a, mut b) = (image(0), image(0));
    let mut slots = Slots::for_tests([&mut a, &mut b], W, H);
    let full = Rect::new(0, 0, W, H);
    frame(&mut slots, 0, full, &image(1));
    frame(&mut slots, 1, full, &image(1));
    let view = Rect::new(0, 1, W, 5);
    frame(&mut slots, 0, view, &image(2));
    // A scrolling view: every frame repaints it whole.
    assert_eq!(frame(&mut slots, 1, view, &image(3)), view);
    drop(slots);
    assert_eq!(pixel(&b, 0, 0), [1; 4], "outside the view: untouched");
    assert_eq!(pixel(&b, 4, 3), [3; 4]);
}

#[test]
fn a_slot_of_another_size_supplies_nothing() {
    let (mut a, mut b) = (image(0), image(0));
    let mut slots = Slots::for_tests([&mut a, &mut b], W, H);
    let full = Rect::new(0, 0, W, H);
    frame(&mut slots, 0, full, &image(1));
    frame(&mut slots, 1, full, &image(1));
    frame(&mut slots, 0, Rect::new(0, 0, W, 2), &image(2));
    // As if slot 0 were still at the window's old size.
    slots.slots[0].width = W - 1;
    assert_eq!(
        slots.prepare(1, Rect::new(0, 4, W, 5)),
        Some(Rect::new(0, 0, W, 5))
    );
}

#[test]
fn an_invalidated_slot_takes_everything_back_from_the_other() {
    let (mut a, mut b) = (image(0), image(0));
    let mut slots = Slots::for_tests([&mut a, &mut b], W, H);
    let full = Rect::new(0, 0, W, H);
    frame(&mut slots, 0, full, &image(1));
    frame(&mut slots, 1, full, &image(1));
    // A frame fails part way through slot 0, leaving garbage in it.
    slots.prepare(0, Rect::new(0, 0, 1, 1));
    assert!(slots.write(0, &image(9), W, H, full));
    slots.invalidate(0);
    // The retried damage is composed; slot 1 is current everywhere else.
    let pixel_rect = Rect::new(0, 0, 1, 1);
    assert_eq!(slots.prepare(0, pixel_rect), Some(pixel_rect));
    assert!(slots.write(0, &image(2), W, H, pixel_rect));
    drop(slots);
    assert_eq!(pixel(&a, 0, 0), [2; 4]);
    assert_eq!(pixel(&a, 5, 4), [1; 4], "copied back from slot 1");
}

#[test]
fn damage_outside_the_image_is_clipped() {
    let (mut a, mut b) = (image(0), image(0));
    let mut slots = Slots::for_tests([&mut a, &mut b], W, H);
    let full = Rect::new(0, 0, W, H);
    assert_eq!(slots.prepare(0, Rect::new(-10, -10, 100, 100)), Some(full));
    assert_eq!(slots.prepare(0, Rect::new(100, 100, 120, 120)), None);
}

#[test]
fn a_wrong_size_image_is_refused() {
    let (mut a, mut b) = (image(0), image(0));
    let mut slots = Slots::for_tests([&mut a, &mut b], W, H);
    let full = Rect::new(0, 0, W, H);
    assert!(!slots.write(0, &[5; 16], W, H, full));
    assert!(!slots.write(0, &[5; 16], 2, 2, full), "not the slot's size");
    assert!(slots.mapping(0, W, H).is_some());
    assert!(slots.mapping(0, 2, 2).is_none());
    drop(slots);
    assert!(a.iter().all(|&byte| byte == 0));
}

#[test]
fn a_cancelled_present_frees_the_slot_and_keeps_refusal_tracking() {
    let (mut a, mut b) = (image(0), image(0));
    let mut slots = Slots::for_tests([&mut a, &mut b], W, H);
    assert_eq!(slots.submit(0), Some(1));
    assert_eq!(slots.submit(1), Some(2));
    assert!(slots.cancel(1, 2));
    assert!(!slots.cancel(1, 2), "only once");
    // Slot 0 is the newest present again: its release is a refusal.
    assert!(slots.released(0));
    assert_eq!(slots.submit(1), Some(2), "slot 1 is free again");
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

/// A tiny deterministic generator for the model test.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, below: i32) -> i32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) % below.max(1) as u64) as i32
    }

    fn rect(&mut self) -> Rect {
        let (x, y) = (self.next(W), self.next(H));
        Rect::new(x, y, x + 1 + self.next(W - x), y + 1 + self.next(H - y))
    }
}

/// Random frames into random slots, some failing part way (garbage written,
/// slot invalidated, damage kept for the next frame): every slot composed
/// must equal the picture, with only its damage composed when another slot
/// could supply the rest.
#[test]
fn seeded_frames_keep_every_composed_slot_equal_to_the_picture() {
    for seed in 0..200u64 {
        let (mut a, mut b) = (image(0), image(0));
        let (pa, pb) = (a.as_mut_ptr(), b.as_mut_ptr());
        let mut slots = Slots::for_tests([&mut a, &mut b], W, H);
        let mut rng = Lcg(seed);
        let mut picture = image(1);
        let mut pending: Option<Rect> = None;
        for step in 0..60u8 {
            let change = rng.rect();
            paint(&mut picture, change, step.wrapping_add(2));
            let damage = pending.map_or(change, |p| union(p, change));
            let slot = rng.next(SLOTS as i32) as u32;
            let compose = slots.prepare(slot, damage).expect("in the image");
            if rng.next(8) == 0 {
                assert!(slots.write(slot, &image(0xEE), W, H, compose));
                slots.invalidate(slot);
                pending = Some(damage);
                continue;
            }
            pending = None;
            assert!(slots.write(slot, &picture, W, H, compose));
            let ptr = if slot == 0 { pa } else { pb };
            // SAFETY: the buffer is `BYTES` long and not written while read.
            let shown = unsafe { core::slice::from_raw_parts(ptr, BYTES) };
            assert_eq!(shown, picture.as_slice(), "seed {seed} step {step}");
        }
    }
}
