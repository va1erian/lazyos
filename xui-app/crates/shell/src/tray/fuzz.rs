//! Seeded fuzz over random set/update/clear/resident-topic/order/hidden
//! sequences (docs/tray-plan.md section 8), with the model's invariants
//! checked after every step. `FUZZ_CASES` / `FUZZ_SEED` as in `fuzzkit`.

use fuzzkit::{for_seeds, Rng};
use messenger_generated::os_lazy_shell_tray_v1 as wire;

use super::item::{Item, Patch};
use super::layout::{self, CELL, TRAY_VISIBLE};
use super::{Change, Refused, Tray, ITEMS_MAX};
use crate::taskbar::ENTRY_X;

/// More apps than the tray holds, so `Full` is reached.
const APPS: u64 = 80;

fn app(rng: &mut Rng) -> String {
    format!("org.app{}", rng.below(APPS))
}

fn text(rng: &mut Rng, max: u64) -> String {
    let len = rng.below(max);
    (0..len)
        .map(|_| char::from(rng.range(1, 0x7f) as u8))
        .collect()
}

fn image(rng: &mut Rng) -> wire::Image {
    let (w, h) = (rng.below(70) as u32, rng.below(70) as u32);
    let mut len = (w * h * 4) as usize;
    if rng.one_in(4) {
        len = rng.below(64) as usize;
    }
    wire::Image {
        width: w,
        height: h,
        data: rng.bytes(len),
    }
}

fn icon(rng: &mut Rng) -> wire::Icon {
    let mut icon = wire::Icon::default();
    for _ in 0..rng.below(3) {
        match rng.below(4) {
            0 => icon.lucide = Some(text(rng, 12)),
            1 => icon.mask = Some(image(rng)),
            2 => icon.pixels = (0..rng.below(4)).map(|_| image(rng)).collect(),
            _ => icon.file = Some(format!("{}.png", text(rng, 8))),
        }
    }
    icon
}

fn menu(rng: &mut Rng) -> Vec<wire::MenuItem> {
    (0..rng.below(70))
        .map(|_| wire::MenuItem {
            id: rng.below(80) as u32,
            parent: if rng.one_in(3) {
                rng.below(80) as u32
            } else {
                0
            },
            label: text(rng, 140),
            kind: rng.below(6) as u32,
            enabled: rng.one_in(2),
            checked: rng.one_in(2),
            is_default: rng.one_in(8),
        })
        .collect()
}

fn item(rng: &mut Rng) -> wire::Item {
    wire::Item {
        icon: icon(rng),
        tooltip: text(rng, 300),
        status: rng.below(4) as u32,
        badge: rng.one_in(2).then(|| text(rng, 5)),
        menu: menu(rng),
        activate: rng.below(4) as u32,
    }
}

fn update(rng: &mut Rng) -> wire::UpdateArgs {
    wire::UpdateArgs {
        icon: rng.one_in(2).then(|| icon(rng)),
        tooltip: rng.one_in(2).then(|| text(rng, 300)),
        status: rng.one_in(2).then(|| rng.below(4) as u32),
        badge: rng.one_in(2).then(|| text(rng, 5)),
        menu: rng.one_in(3).then(|| wire::Menu { rows: menu(rng) }),
    }
}

fn step(tray: &mut Tray, rng: &mut Rng) {
    let target = app(rng);
    let before = tray.len();
    match rng.below(7) {
        0 | 1 => {
            if let Ok(item) = Item::from_wire(item(rng)) {
                match tray.set(&target, item) {
                    Ok(Change::Added) => assert_eq!(tray.len(), before + 1),
                    Ok(_) => assert_eq!(tray.len(), before),
                    Err(refused) => {
                        assert_eq!(refused, Refused::Full);
                        assert_eq!(before, ITEMS_MAX);
                    }
                }
            }
        }
        2 => {
            if let Ok(patch) = Patch::from_wire(update(rng)) {
                let had = tray.get(&target).is_some_and(|e| e.custom.is_some());
                let result = tray.update(&target, patch);
                assert!(had || result == Err(Refused::NoItem));
            }
        }
        3 => {
            let change = tray.clear(&target);
            assert!(tray.get(&target).is_none_or(|e| e.custom.is_none()));
            assert!(change != Change::Added && change != Change::Replaced);
        }
        4 => {
            let running: Vec<(String, u64)> = (0..rng.below(12))
                .map(|_| (app(rng), rng.below(1000)))
                .collect();
            tray.set_resident(&running);
        }
        5 => tray.set_order((0..rng.below(10)).map(|_| app(rng)).collect()),
        _ => tray.set_hidden((0..rng.below(10)).map(|_| app(rng)).collect()),
    }
}

fn check(tray: &Tray, clock_x: i32) {
    let entries = tray.entries();
    assert!(entries.len() <= ITEMS_MAX);
    for (i, entry) in entries.iter().enumerate() {
        // One item per app, and nothing without a reason to be there.
        assert!(entries[i + 1..].iter().all(|other| other.app != entry.app));
        assert!(entry.custom.is_some() || entry.resident.is_some());
    }
    let layout = layout::layout(tray, clock_x);
    assert!(layout.cells.len() <= TRAY_VISIBLE);
    assert_eq!(layout.cells.len() + layout.overflow.len(), entries.len());
    assert_eq!(layout.chevron.is_some(), !layout.overflow.is_empty());
    for (i, cell) in layout.cells.iter().enumerate() {
        assert!(!layout.overflow.contains(&cell.app));
        assert!(!tray.is_hidden(&cell.app));
        assert!(cell.rect.x >= ENTRY_X && cell.rect.x + CELL <= clock_x);
        if let Some(next) = layout.cells.get(i + 1) {
            assert_eq!(next.rect.x, cell.rect.x + CELL);
        }
    }
}

#[test]
fn seeded_sequences_keep_the_tray_consistent() {
    for_seeds("seeded_sequences_keep_the_tray_consistent", |_, rng| {
        let mut tray = Tray::new();
        let clock_x = rng.range(200, 2600) as i32;
        for _ in 0..rng.range(1, 200) {
            step(&mut tray, rng);
            check(&tray, clock_x);
        }
    });
}
