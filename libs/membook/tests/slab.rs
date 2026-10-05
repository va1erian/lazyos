//! Slab classes and free lists over real heap pages (so Miri checks every
//! link the list writes into a slot), plus a seeded soak against a model.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::collections::HashSet;
use std::ptr::NonNull;

use membook::slab::{
    class_for_layout, class_for_size, slots_per_slab, Class, DoubleFree, Owner, CLASSES,
    CLASS_COUNT, FRAME_SIZE, MAX_SLAB_SIZE,
};

/// Page-aligned pages, freed when the test ends.
struct Pages(Vec<NonNull<u8>>);

const PAGE: Layout = match Layout::from_size_align(FRAME_SIZE, FRAME_SIZE) {
    Ok(layout) => layout,
    Err(_) => panic!("page layout"),
};

impl Pages {
    fn new() -> Self {
        Pages(Vec::new())
    }

    fn take(&mut self) -> NonNull<u8> {
        // SAFETY: PAGE has a non-zero size.
        let page = NonNull::new(unsafe { alloc_zeroed(PAGE) }).expect("out of memory");
        self.0.push(page);
        page
    }

    fn owns(&self, slot: NonNull<u8>) -> bool {
        let at = slot.as_ptr() as usize;
        self.0
            .iter()
            .any(|page| (page.as_ptr() as usize..page.as_ptr() as usize + FRAME_SIZE).contains(&at))
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        for page in &self.0 {
            // SAFETY: every page came from `alloc_zeroed(PAGE)`.
            unsafe { dealloc(page.as_ptr(), PAGE) };
        }
    }
}

/// A class with one carved page.
fn carved(pages: &mut Pages, class: usize) -> Class {
    let mut list = Class::new();
    // SAFETY: a fresh page-aligned page, handed to this class only.
    unsafe { list.carve(pages.take(), class) };
    list
}

#[test]
fn sizes_pick_the_smallest_class() {
    assert_eq!(class_for_size(0), Some(0));
    assert_eq!(class_for_size(1), Some(0));
    assert_eq!(class_for_size(32), Some(0));
    assert_eq!(class_for_size(33), Some(1));
    assert_eq!(class_for_size(2048), Some(6));
    assert_eq!(class_for_size(MAX_SLAB_SIZE), Some(CLASS_COUNT - 1));
    assert_eq!(class_for_size(MAX_SLAB_SIZE + 1), None);
    assert_eq!(class_for_size(usize::MAX), None);
    // Alignment counts: a 1-byte object aligned to 256 needs the 256 class.
    assert_eq!(class_for_layout(1, 256), Some(3));
    assert_eq!(class_for_layout(0, 1), Some(0));
    assert_eq!(class_for_layout(8, 8192), None);
    for (class, &size) in CLASSES.iter().enumerate() {
        assert_eq!(slots_per_slab(class) * size, FRAME_SIZE);
    }
}

#[test]
fn a_carved_page_hands_out_every_slot_once() {
    let mut pages = Pages::new();
    for (class, &size) in CLASSES.iter().enumerate() {
        let mut list = carved(&mut pages, class);
        assert_eq!(list.slabs, 1);
        assert_eq!(list.free_slots(class), slots_per_slab(class));
        let mut seen = HashSet::new();
        while let Some(slot) = list.alloc() {
            assert!(pages.owns(slot));
            assert_eq!(slot.as_ptr() as usize % size, 0, "misaligned slot");
            assert!(
                seen.insert(slot.as_ptr() as usize),
                "a slot was handed out twice"
            );
            // The slot is ours: write all of it, links included.
            // SAFETY: a handed-out slot is `size` bytes we own.
            unsafe { slot.as_ptr().write_bytes(0xA5, size) };
        }
        assert_eq!(seen.len(), slots_per_slab(class));
        assert_eq!(list.live, slots_per_slab(class));
        assert_eq!(list.peak, list.live);
        assert!(!list.has_free());
        assert_eq!(list.free_slots(class), 0);
    }
}

#[test]
fn frees_are_reused_last_in_first_out() {
    let mut pages = Pages::new();
    let mut list = carved(&mut pages, 2);
    let a = list.alloc().unwrap();
    let b = list.alloc().unwrap();
    // SAFETY: both came from this class and are not used again.
    unsafe {
        list.free(a).unwrap();
        list.free(b).unwrap();
    }
    assert_eq!(list.alloc(), Some(b));
    assert_eq!(list.alloc(), Some(a));
    assert_eq!(
        (list.allocations, list.frees, list.live, list.peak),
        (4, 2, 2, 2)
    );
}

#[test]
fn a_free_with_nothing_live_is_refused_and_changes_nothing() {
    let mut pages = Pages::new();
    let mut list = carved(&mut pages, 0);
    let slot = list.alloc().unwrap();
    // SAFETY: returned once, legitimately.
    unsafe { list.free(slot).unwrap() };
    let before = (list.live, list.frees, list.free_slots(0));
    // SAFETY: the refused free never touches the slot (that is the point).
    assert_eq!(unsafe { list.free(slot) }, Err(DoubleFree));
    assert_eq!((list.live, list.frees, list.free_slots(0)), before);
}

#[test]
fn an_empty_class_grows_by_carving() {
    let mut pages = Pages::new();
    let mut list = Class::new();
    assert!(!list.has_free());
    assert_eq!(list.alloc(), None);
    assert_eq!(list.pop(), None);
    // SAFETY: fresh pages for this class only.
    unsafe {
        list.carve(pages.take(), 7);
        list.carve(pages.take(), 7);
    }
    assert_eq!(list.slabs, 2);
    assert_eq!(list.free_slots(7), 2);
    let first = list.alloc().unwrap();
    let second = list.alloc().unwrap();
    assert_ne!(first, second);
    assert_eq!(list.alloc(), None);
}

#[test]
fn owners_charge_and_saturate() {
    let mut owner = Owner::new();
    owner.charge(100);
    owner.charge(50);
    assert!(owner.uncharge(30));
    assert_eq!(
        (owner.live, owner.peak, owner.charges, owner.uncharges),
        (120, 150, 2, 1)
    );
    assert!(!owner.uncharge(121), "an over-uncharge succeeded");
    assert_eq!(owner.live, 0, "an over-uncharge left a balance");
    owner.charge(usize::MAX);
    owner.charge(1);
    assert_eq!(owner.live, usize::MAX, "the charge wrapped");
}

/// xorshift64*: deterministic, so a failure replays.
struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % bound as u64) as usize
    }
}

/// Soak: random allocations and frees across every class, carving pages on
/// demand, checked against a model after every step: no slot is ever handed
/// out twice, every slot is inside a carved page, the counters match the
/// model, and a slot's contents survive until it is freed.
#[test]
fn soak_random_churn_matches_a_model() {
    let steps = if cfg!(miri) { 1_500 } else { 400_000 };
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut pages = Pages::new();
    let mut lists: Vec<Class> = (0..CLASS_COUNT).map(|_| Class::new()).collect();
    let mut live: Vec<Vec<(NonNull<u8>, u8)>> = vec![Vec::new(); CLASS_COUNT];
    let mut handed = HashSet::new();
    for step in 0..steps {
        let class = rng.below(CLASS_COUNT);
        let free = !live[class].is_empty() && rng.below(100) < 48;
        if free {
            let pick = rng.below(live[class].len());
            let (slot, tag) = live[class].swap_remove(pick);
            // SAFETY: the slot is ours and CLASSES[class] bytes long.
            let bytes = unsafe { std::slice::from_raw_parts(slot.as_ptr(), CLASSES[class]) };
            assert!(
                bytes.iter().all(|&b| b == tag),
                "step {step}: a live slot was clobbered"
            );
            assert!(handed.remove(&(slot.as_ptr() as usize)));
            // SAFETY: allocated from this class, freed once.
            unsafe { lists[class].free(slot).unwrap() };
        } else {
            if !lists[class].has_free() {
                // SAFETY: a fresh page for this class only.
                unsafe { lists[class].carve(pages.take(), class) };
            }
            let slot = lists[class].alloc().expect("a carved class has a slot");
            assert!(pages.owns(slot), "step {step}: a slot outside every page");
            assert!(
                handed.insert(slot.as_ptr() as usize),
                "step {step}: handed out twice"
            );
            let tag = (step % 251) as u8;
            // SAFETY: a handed-out slot is CLASSES[class] bytes we own.
            unsafe { slot.as_ptr().write_bytes(tag, CLASSES[class]) };
            live[class].push((slot, tag));
        }
        let list = &lists[class];
        assert_eq!(list.live, live[class].len(), "step {step}: live count");
        assert_eq!(
            list.allocations - list.frees,
            list.live,
            "step {step}: counters"
        );
        assert!(list.peak >= list.live);
    }
    // Drain: every class returns to all-free, with no slot lost.
    for class in 0..CLASS_COUNT {
        for (slot, _) in live[class].drain(..) {
            // SAFETY: allocated from this class, freed once.
            unsafe { lists[class].free(slot).unwrap() };
        }
        let list = &mut lists[class];
        assert_eq!(list.live, 0);
        let mut free = 0;
        while list.pop().is_some() {
            free += 1;
        }
        assert_eq!(
            free,
            list.slabs * slots_per_slab(class),
            "class {class} lost slots"
        );
    }
}
