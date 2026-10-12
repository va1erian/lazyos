#![forbid(unsafe_code)]

//! Selections on the model: the commands (select all, invert, ...), the four
//! selection tools' drags, and Erase / Fill Selection.
//!
//! A selection tool changes the document's selection live while the pointer
//! drags, so the ants follow it, and records one `Edit::Selection` on release.

use super::{Drag, Model};
use crate::model::history::UndoCapture;
use crate::model::selection::mix_by;
use crate::model::{
    CombineMode, Edit, Mods, Pixel, Rect, Selection, Side, Tool, fill, source_over,
};

/// Pointer coordinates are clamped to this magnitude so shape arithmetic
/// cannot overflow, however far a drag strays.
const REACH: i64 = 1 << 24;

impl Model {
    /// The selection, if there is one.
    pub fn selection(&self) -> Option<&Selection> {
        self.doc.selection()
    }

    /// A counter that changes whenever the selection may have, for caches of
    /// its outline.
    pub fn selection_rev(&self) -> u64 {
        self.sel_rev
    }

    /// How a new selection joins the old one without a modifier.
    pub fn sel_mode(&self) -> CombineMode {
        self.sel_mode
    }

    /// Sets the default combine mode (the options bar).
    pub fn set_sel_mode(&mut self, mode: CombineMode) {
        self.sel_mode = mode;
    }

    /// The Magic Wand's tolerance in percent.
    pub fn wand_tolerance(&self) -> u8 {
        self.wand_tolerance
    }

    /// Sets the Magic Wand's tolerance, clamped to 0-100.
    pub fn set_wand_tolerance(&mut self, percent: u8) {
        self.wand_tolerance = percent.min(100);
    }

    /// Whether the Magic Wand selects only the region around the click.
    pub fn wand_contiguous(&self) -> bool {
        self.wand_contiguous
    }

    /// Chooses between a contiguous region and every similar pixel.
    pub fn set_wand_contiguous(&mut self, contiguous: bool) {
        self.wand_contiguous = contiguous;
    }

    /// Takes the reason the last tool action did nothing, if there was one.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    pub(super) fn selection_changed(&mut self) {
        self.sel_rev = self.sel_rev.wrapping_add(1);
        self.bump();
    }

    /// Makes `new` the selection as one undo step named `name`; `false` (and
    /// no step) when it already is.
    pub(super) fn change_selection(&mut self, name: &str, new: Option<Selection>) -> bool {
        if self.doc.selection() == new.as_ref() {
            return false;
        }
        let before = self.doc.set_selection(new);
        self.history.record(Edit::Selection {
            name: name.to_string(),
            before,
        });
        self.selection_changed();
        true
    }

    /// Selects the whole picture.
    pub fn select_all(&mut self) {
        self.settle();
        let all = Selection::rect(self.doc.bounds());
        self.change_selection("Select All", all);
    }

    /// Drops the selection.
    pub fn deselect(&mut self) {
        self.settle();
        self.change_selection("Deselect All", None);
    }

    /// Selects what is not selected; with nothing selected there is nothing
    /// to invert.
    pub fn invert_selection(&mut self) {
        self.settle();
        let Some(current) = self.doc.selection() else {
            return;
        };
        let inverted = current.inverted_within(self.doc.bounds());
        self.change_selection("Invert Selection", inverted);
    }

    /// The combine mode a drag uses: Ctrl adds, the right button removes
    /// (with Ctrl: toggles), Alt with the left button intersects; with none
    /// of them the options bar's mode.
    fn resolve_mode(&self, side: Side, mods: Mods) -> CombineMode {
        match (side, mods.ctrl, mods.alt) {
            (Side::Secondary, true, _) => CombineMode::Xor,
            (Side::Secondary, false, _) => CombineMode::Exclude,
            (Side::Primary, _, true) => CombineMode::Intersect,
            (Side::Primary, true, false) => CombineMode::Union,
            (Side::Primary, false, false) => self.sel_mode,
        }
    }

    /// Shows `shape` joined to `base` as the selection, without a history step.
    fn show_selection(
        &mut self,
        base: &Option<Selection>,
        shape: Option<&Selection>,
        mode: CombineMode,
    ) {
        let new = Selection::combine(base.as_ref(), shape, mode);
        if self.doc.selection() != new.as_ref() {
            self.doc.set_selection(new);
            self.selection_changed();
        }
    }

    /// A selection tool's press.
    pub(super) fn begin_select(&mut self, x: i32, y: i32, side: Side, mods: Mods) {
        let mode = self.resolve_mode(side, mods);
        let base = self.doc.selection().cloned();
        match self.tool {
            Tool::MagicWand => self.wand(x, y, mode),
            Tool::Lasso => {
                self.show_selection(&base, None, mode);
                self.drag = Some(Drag::Lasso {
                    base,
                    mode,
                    points: vec![(x, y)],
                });
            }
            _ => {
                // A click without a drag selects nothing: Replace deselects.
                self.show_selection(&base, None, mode);
                self.drag = Some(Drag::Select {
                    base,
                    mode,
                    start: (x, y),
                    moved: false,
                });
            }
        }
    }

    /// The drag of a rectangle, ellipse or lasso moved to `(x, y)`.
    pub(super) fn extend_select(&mut self, drag: Drag, x: i32, y: i32, mods: Mods) -> Drag {
        match drag {
            Drag::Select {
                base,
                mode,
                start,
                moved,
            } => {
                let moved = moved || (x, y) != start;
                if moved {
                    let shape = self.drag_shape(start, (x, y), mods.shift);
                    self.show_selection(&base, shape.as_ref(), mode);
                }
                Drag::Select {
                    base,
                    mode,
                    start,
                    moved,
                }
            }
            Drag::Lasso {
                base,
                mode,
                mut points,
            } => {
                if points.last() != Some(&(x, y)) {
                    points.push((x, y));
                    let shape = Selection::polygon(&points, self.doc.bounds());
                    self.show_selection(&base, shape.as_ref(), mode);
                }
                Drag::Lasso { base, mode, points }
            }
            other => other,
        }
    }

    /// Records the selection a finished drag left.
    pub(super) fn end_select(&mut self, base: Option<Selection>) {
        // The drag already made the new selection current, so the base is
        // what the step swaps back in.
        if self.doc.selection() != base.as_ref() {
            let name = self.tool.label().to_string();
            self.history.record(Edit::Selection { name, before: base });
        }
        self.selection_changed();
    }

    /// The rectangle or ellipse from `start` to `end`, a square or circle with
    /// `square`, clipped to the picture.
    fn drag_shape(&self, start: (i32, i32), end: (i32, i32), square: bool) -> Option<Selection> {
        let clamp = |v: i32| i64::from(v).clamp(-REACH, REACH);
        let (sx, sy) = (clamp(start.0), clamp(start.1));
        let (mut ex, mut ey) = (clamp(end.0), clamp(end.1));
        if square {
            let side = (ex - sx).abs().max((ey - sy).abs());
            ex = sx + if ex < sx { -side } else { side };
            ey = sy + if ey < sy { -side } else { side };
        }
        let bounds = Rect::from_points((sx as i32, sy as i32), (ex as i32, ey as i32));
        match self.tool {
            Tool::EllipseSelect => Selection::ellipse(bounds, self.doc.bounds()),
            _ => Selection::rect(bounds.intersect(self.doc.bounds())),
        }
    }

    /// The Magic Wand: selects the pixels of the active layer like the one
    /// clicked, within the tolerance, joined to the selection by `mode`.
    fn wand(&mut self, x: i32, y: i32, mode: CombineMode) {
        let threshold = fill::tolerance_threshold(self.wand_tolerance);
        let spans = fill::similar_spans(
            &self.doc.active_layer().pixels,
            x,
            y,
            threshold,
            self.wand_contiguous,
        );
        let shape = spans.and_then(|spans| Selection::from_spans(&spans, self.doc.bounds()));
        let new = Selection::combine(self.doc.selection(), shape.as_ref(), mode);
        self.change_selection("Magic Wand", new);
    }

    /// Fills the selection (the whole layer when nothing is selected) with
    /// `color`, composited over what is there, as one undo step.
    pub fn fill_selection(&mut self, color: Pixel) {
        self.paint_selection("Fill Selection", move |old| source_over(old, color, 255));
    }

    /// Makes the selection (the whole layer when nothing is selected)
    /// transparent as one undo step.
    pub fn erase_selection(&mut self) {
        self.paint_selection("Erase Selection", |_| [0, 0, 0, 0]);
    }

    /// Replaces each selected pixel of the active layer by `paint(pixel)`,
    /// weighted by the coverage.
    pub(super) fn paint_selection(&mut self, name: &str, paint: impl Fn(Pixel) -> Pixel) {
        self.settle();
        let layer = self.doc.active();
        let mut capture = UndoCapture::new();
        let Some((target, selection)) = self.doc.layer_and_selection(layer) else {
            return;
        };
        let area = selection.map_or(target.pixels.rect(), Selection::bounds);
        let area = area.intersect(target.pixels.rect());
        capture.touch(&target.pixels, area);
        for py in area.y0..area.y1 {
            for px in area.x0..area.x1 {
                let cover = selection.map_or(255, |s| s.coverage(px, py));
                if let Some(old) = target.pixels.get(px, py) {
                    target.pixels.put(px, py, mix_by(cover, old, paint(old)));
                }
            }
        }
        self.commit_named(layer, capture, name);
    }
}
