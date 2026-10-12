//! Selections on the model, part three: the clipboard, crop, undo during a
//! float and the seeded fuzz. No window, no `Ui`.

#![allow(dead_code, unused_imports)]

use xui_paint::model::{Bitmap, CombineMode, Model, Mods, Rect, Side, Tool, WHITE};

const RED: [u8; 4] = [255, 0, 0, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];
const NONE: Mods = Mods::NONE;
const CTRL: Mods = Mods {
    ctrl: true,
    ..Mods::NONE
};
const ALT: Mods = Mods {
    alt: true,
    ..Mods::NONE
};
const SHIFT: Mods = Mods {
    shift: true,
    ..Mods::NONE
};

/// One drag of `tool` from `a` to `b` with `mods` and `side`.
fn drag(m: &mut Model, tool: Tool, a: (i32, i32), b: (i32, i32), side: Side, mods: Mods) {
    m.set_tool(tool);
    m.begin_with(a.0, a.1, side, mods);
    m.extend_with(b.0, b.1, mods);
    m.end();
}

fn select(m: &mut Model, tool: Tool, a: (i32, i32), b: (i32, i32)) {
    drag(m, tool, a, b, Side::Primary, NONE);
}

fn sel_bounds(m: &Model) -> Option<Rect> {
    m.selection().map(|s| s.bounds())
}

fn selected(m: &Model, x: i32, y: i32) -> bool {
    m.selection().is_some_and(|s| s.contains(x, y))
}

/// A model whose layer has a red 10x10 block at (5, 5), the block selected.
fn with_block() -> Model {
    let mut m = Model::new(2, 2);
    let mut bitmap = Bitmap::white(40, 40);
    for y in 5..15 {
        for x in 5..15 {
            bitmap.put(x, y, RED);
        }
    }
    m.load(bitmap).unwrap();
    select(&mut m, Tool::RectSelect, (5, 5), (14, 14));
    m
}

#[test]
fn copy_cut_and_paste_round_trip() {
    let mut m = with_block();
    assert!(
        Model::new(4, 4).copy(false).is_err(),
        "copy needs a selection"
    );
    m.copy(false).unwrap();
    let clip = m.clipboard().unwrap();
    assert_eq!(clip.bitmap().size(), (10, 10));
    assert_eq!(clip.offset(), (5, 5));
    assert_eq!(clip.bitmap().get(0, 0), Some(RED));
    assert!(m.reserved().clipboard > 0);
    assert!(
        !m.history().can_undo() || m.history().undo_name() != Some("Copy"),
        "copy is not an edit"
    );
    // Cut erases and is undoable.
    m.cut().unwrap();
    assert_eq!(m.bitmap().get(8, 8), Some(CLEAR));
    assert_eq!(m.history().undo_name(), Some("Cut"));
    m.undo();
    assert_eq!(m.bitmap().get(8, 8), Some(RED));
    // Paste floats at the requested spot, selects it and takes the move tool.
    m.paste((20, 22)).unwrap();
    assert_eq!(m.tool(), Tool::MovePixels);
    assert!(m.is_floating());
    assert_eq!(sel_bounds(&m), Some(Rect::new(20, 22, 10, 10)));
    assert_eq!(m.bitmap().get(25, 27), Some(RED));
    assert_eq!(m.bitmap().get(5, 5), Some(RED), "the source is still there");
    m.commit_float();
    assert_eq!(m.history().undo_name(), Some("Paste"));
    m.undo();
    assert_eq!(m.bitmap().get(25, 27), Some(WHITE));
    assert_eq!(sel_bounds(&m), Some(Rect::new(5, 5, 10, 10)));
}

#[test]
fn a_masked_copy_is_transparent_outside_and_copy_merged_sees_every_layer() {
    let mut m = Model::new(30, 30);
    select(&mut m, Tool::EllipseSelect, (0, 0), (19, 19));
    m.copy(false).unwrap();
    let clip = m.clipboard().unwrap().bitmap().clone();
    assert_eq!(clip.get(0, 0), Some(CLEAR));
    assert_eq!(clip.get(10, 10), Some(WHITE));
    // A top layer with a red dot: Copy takes the active layer only.
    m.add_layer().unwrap();
    m.set_tool(Tool::Pencil);
    m.set_primary(RED);
    m.select_layer(0);
    select(&mut m, Tool::RectSelect, (0, 0), (9, 9));
    m.select_layer(1);
    m.set_tool(Tool::Pencil);
    m.begin(3, 3, Side::Primary);
    m.end();
    m.copy(false).unwrap();
    assert_eq!(m.clipboard().unwrap().bitmap().get(3, 3), Some(RED));
    assert_eq!(m.clipboard().unwrap().bitmap().get(5, 5), Some(CLEAR));
    m.select_layer(0);
    m.copy(true).unwrap();
    assert_eq!(
        m.clipboard().unwrap().bitmap().get(3, 3),
        Some(RED),
        "merged shows the red dot"
    );
    assert_eq!(
        m.clipboard().unwrap().bitmap().get(5, 5),
        Some(WHITE),
        "and the background"
    );
}

#[test]
fn paste_into_a_new_layer_and_a_new_image() {
    let mut m = with_block();
    assert!(m.paste_new_layer((0, 0)).is_err(), "nothing copied yet");
    assert!(m.paste_new_image().is_err());
    m.copy(false).unwrap();
    m.paste_new_layer((20, 20)).unwrap();
    assert_eq!(m.document().layers().len(), 2);
    assert_eq!(m.document().active(), 1);
    m.commit_float();
    assert_eq!(m.document().layer(1).unwrap().pixels.get(25, 25), Some(RED));
    assert_eq!(
        m.document().layer(0).unwrap().pixels.get(25, 25),
        Some(WHITE)
    );
    m.undo();
    m.undo();
    assert_eq!(m.document().layers().len(), 1);
    m.paste_new_image().unwrap();
    assert_eq!(m.document().size(), (10, 10));
    assert_eq!(m.bitmap().get(4, 4), Some(RED));
    assert!(
        m.clipboard().is_some(),
        "the clipboard outlives the document"
    );
    assert!(m.selection().is_none());
}

#[test]
fn a_copy_over_the_budget_is_refused_and_keeps_the_old_clipboard() {
    let mut m = with_block();
    m.copy(false).unwrap();
    let before = m.clipboard().unwrap().clone();
    select(&mut m, Tool::RectSelect, (0, 0), (30, 30));
    m.set_floating_bytes(500 * 1024 * 1024);
    let error = m.copy(false).unwrap_err();
    assert!(error.contains("not enough memory"), "{error}");
    assert_eq!(m.clipboard(), Some(&before));
    assert!(m.cut().is_err());
    assert_eq!(
        m.bitmap().get(0, 0),
        Some(WHITE),
        "a refused cut erases nothing"
    );
    m.set_floating_bytes(0);
    m.clear_clipboard();
    assert!(m.clipboard().is_none() && m.reserved().clipboard == 0);
}

#[test]
fn crop_to_selection_is_one_undoable_document_edit() {
    let mut m = with_block();
    assert!(m.crop_to_selection().is_ok());
    assert_eq!(m.document().size(), (10, 10));
    assert!(m.selection().is_none());
    assert_eq!(m.bitmap().get(0, 0), Some(RED));
    assert_eq!(m.history().undo_name(), Some("Crop to Selection"));
    m.undo();
    assert_eq!(m.document().size(), (40, 40));
    assert_eq!(
        sel_bounds(&m),
        Some(Rect::new(5, 5, 10, 10)),
        "the selection comes back"
    );
    m.redo();
    assert_eq!(m.document().size(), (10, 10));
    assert!(m.crop_to_selection().is_err(), "nothing selected now");
    // An elliptical crop clears its corners on every layer.
    let mut m = Model::new(30, 30);
    select(&mut m, Tool::EllipseSelect, (5, 5), (24, 24));
    m.crop_to_selection().unwrap();
    assert_eq!(m.bitmap().get(0, 0), Some(CLEAR));
    assert_eq!(m.bitmap().get(10, 10), Some(WHITE));
}

#[test]
fn undo_during_a_float_lands_it_first() {
    let mut m = with_block();
    m.set_tool(Tool::MovePixels);
    m.begin_with(8, 8, Side::Primary, NONE);
    m.extend_with(9, 8, NONE);
    m.end();
    m.undo();
    assert!(!m.is_floating());
    assert_eq!(
        m.bitmap().get(5, 5),
        Some(RED),
        "the move itself was undone"
    );
    m.redo();
    assert_eq!(m.bitmap().get(5, 5), Some(CLEAR));
}

/// A tiny xorshift for the seeded fuzz.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> i32 {
        (self.next() % n) as i32
    }
}

/// One random operation on `m`.
fn random_op(m: &mut Model, rng: &mut Rng) {
    let point = |rng: &mut Rng| (rng.below(34) - 5, rng.below(34) - 5);
    let mods = |rng: &mut Rng| Mods {
        ctrl: rng.next().is_multiple_of(4),
        shift: rng.next().is_multiple_of(4),
        alt: rng.next().is_multiple_of(4),
    };
    let side = |rng: &mut Rng| {
        if rng.next().is_multiple_of(5) {
            Side::Secondary
        } else {
            Side::Primary
        }
    };
    match rng.below(18) {
        0..=3 => {
            let tools = [
                Tool::RectSelect,
                Tool::EllipseSelect,
                Tool::Lasso,
                Tool::MagicWand,
                Tool::MoveSelection,
            ];
            let tool = tools[rng.below(5) as usize];
            m.set_tool(tool);
            let (a, b, c) = (point(rng), point(rng), point(rng));
            let (mod_a, side_a) = (mods(rng), side(rng));
            m.begin_with(a.0, a.1, side_a, mod_a);
            m.extend_with(b.0, b.1, mod_a);
            m.extend_with(c.0, c.1, mod_a);
            m.end();
        }
        4..=6 => {
            let tools = [
                Tool::Pencil,
                Tool::Brush,
                Tool::Eraser,
                Tool::Line,
                Tool::Rectangle,
                Tool::Ellipse,
                Tool::Fill,
            ];
            m.set_tool(tools[rng.below(7) as usize]);
            m.set_size(1 + rng.below(8) as u32);
            let (a, b) = (point(rng), point(rng));
            let s = side(rng);
            m.begin(a.0, a.1, s);
            m.extend(b.0, b.1);
            m.end();
        }
        7 => m.select_all(),
        8 => m.deselect(),
        9 => m.invert_selection(),
        10 => m.fill_selection([rng.below(256) as u8, 40, 90, 255]),
        11 => m.erase_selection(),
        12 => {
            m.set_tool(Tool::MovePixels);
            let (a, b) = (point(rng), point(rng));
            m.begin(a.0, a.1, Side::Primary);
            m.extend(b.0, b.1);
            m.end();
        }
        13 => {
            m.set_tool(Tool::MovePixels);
            m.nudge(rng.below(7) - 3, rng.below(7) - 3);
        }
        14 => {
            let _ = m.copy(rng.next().is_multiple_of(2));
            let at = point(rng);
            let _ = if rng.next().is_multiple_of(2) {
                m.paste(at)
            } else {
                m.paste_new_layer(at)
            };
        }
        15 => {
            let _ = m.cut();
        }
        16 => {
            let _ = m.crop_to_selection();
        }
        _ => {
            if rng.next().is_multiple_of(2) {
                m.cancel_float();
            } else {
                m.commit_float();
            }
        }
    }
}

#[test]
fn seeded_fuzz_of_selection_edit_and_undo_sequences() {
    let cases: u64 = std::env::var("FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    for case in 0..cases {
        let seed = 0x9E37_79B9_7F4A_7C15_u64.wrapping_mul(case + 1);
        let mut rng = Rng(seed);
        let mut m = Model::new(24, 24);
        let initial = m.document().clone();
        for step in 0..40 {
            let before = m.document().clone();
            let (floating, undo_len) = (m.is_floating(), m.history().undo_len());
            random_op(&mut m, &mut rng);
            // Whatever happened, the bookkeeping stays coherent.
            assert_eq!(
                m.reserved().floating > 0,
                m.is_floating(),
                "seed {seed:#x} step {step}"
            );
            if let Some(s) = m.selection() {
                assert!(
                    !s.bounds().is_empty(),
                    "seed {seed:#x} step {step}: empty selection"
                );
                assert_eq!(
                    s.bounds().intersect(m.document().bounds()),
                    s.bounds(),
                    "seed {seed:#x} step {step}: selection leaves the picture"
                );
            }
            // A plain step (no float before or after) undoes exactly.
            if !floating && !m.is_floating() && m.history().undo_len() == undo_len + 1 {
                let after = m.document().clone();
                m.undo();
                assert_eq!(m.document(), &before, "seed {seed:#x} step {step}: undo");
                m.redo();
                assert_eq!(m.document(), &after, "seed {seed:#x} step {step}: redo");
            }
        }
        m.commit_float();
        let last = m.document().clone();
        while m.history().can_undo() {
            m.undo();
        }
        assert_eq!(
            m.document(),
            &initial,
            "seed {seed:#x}: undoing everything restores the start"
        );
        while m.history().can_redo() {
            m.redo();
        }
        assert_eq!(
            m.document(),
            &last,
            "seed {seed:#x}: redoing everything returns"
        );
    }
}
