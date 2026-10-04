//! The cursor as an overlay (docs/performance-plan.md, P3.1).
//!
//! The sprite used to be the last layer of every composition, so a pointer
//! move recomposed the bounding box of the old and new cursor through every
//! layer and presented it: a fast diagonal move redrew a large area. Now the
//! screen buffer holds the composed scene with the sprite stamped on top,
//! and [`CursorOverlay`] keeps the scene pixels the sprite covers
//! ("save-under"). A move restores those pixels at the old place, saves the
//! scene at the new place, draws the sprite there and presents the two small
//! rectangles: nothing is recomposed.
//!
//! The one invariant: while the sprite is shown, `saved` holds exactly the
//! scene under it. Anything that writes the screen buffer (composition, the
//! zoom and resize wireframes) therefore runs between [`CursorOverlay::lift`]
//! and [`CursorOverlay::stamp`], which `Compositor::repaint` and the
//! animation frames do.

use alloc::vec;
use alloc::vec::Vec;
use user::messenger::display::{Canvas, Rect};
use user::sys;

use super::compositor::Compositor;
use super::layout::cursor_rect;

/// Largest UI scale the sprite is drawn at (`Canvas::cursor_scaled` clamps to it).
const MAX_SCALE: usize = 4;
/// Bytes of the largest sprite rectangle: 11x11 design pixels at [`MAX_SCALE`].
const SAVE_BYTES: usize = (11 * MAX_SCALE) * (11 * MAX_SCALE) * 4;

/// Where the sprite is on screen and the scene it hides.
pub(super) struct CursorOverlay {
    /// The rectangle the sprite was stamped into (clipped to the screen) and
    /// the pointer it was drawn for; `None` while nothing is stamped.
    shown: Option<(Rect, (i32, i32))>,
    /// The scene pixels under `shown`, row after row. Allocated once: the
    /// user bump allocator never reclaims.
    saved: Vec<u8>,
}

impl CursorOverlay {
    pub(super) fn new() -> CursorOverlay {
        CursorOverlay {
            shown: None,
            saved: vec![0; SAVE_BYTES],
        }
    }

    /// Put the scene back where the sprite is, so the screen buffer holds
    /// the pure scene; returns the rectangle that changed (empty when the
    /// sprite was not shown).
    pub(super) fn lift(&mut self, screen: &mut Canvas) -> Rect {
        match self.shown.take() {
            Some((rect, _)) => {
                screen.restore(rect, &self.saved);
                rect
            }
            None => Rect::default(),
        }
    }

    /// Save the scene under the sprite at `at` and draw it there; returns the
    /// rectangle that changed.
    pub(super) fn stamp(&mut self, screen: &mut Canvas, at: (i32, i32)) -> Rect {
        let scale = super::theme::scale();
        let saved = screen.save(cursor_rect(at), &mut self.saved);
        if saved.is_empty() {
            return saved;
        }
        screen.cursor_scaled(at.0, at.1, scale, saved);
        self.shown = Some((saved, at));
        saved
    }

    /// Where the sprite is drawn, if it is.
    pub(super) fn position(&self) -> Option<(i32, i32)> {
        self.shown.map(|(_, at)| at)
    }
}

impl Compositor {
    /// Where the cursor belongs now: the newest pointer an animation read,
    /// else the handled one; `None` while the shutdown overlay hides it.
    pub(super) fn cursor_target(&self) -> Option<(i32, i32)> {
        (!super::powerfeed::active()).then(|| self.held.pointer(self.pointer))
    }

    /// Move the sprite to where the pointer is, presenting only the old and
    /// the new sprite rectangles: the cheap path of a plain pointer move.
    pub(super) fn move_cursor(&mut self) {
        let target = self.cursor_target();
        if self.cursor.position() == target {
            return;
        }
        let old = self.cursor.lift(&mut self.screen);
        let new = match target {
            Some(at) => self.cursor.stamp(&mut self.screen, at),
            None => Rect::default(),
        };
        present_cursor(old, new);
    }
}

/// Present the rectangles a sprite move changed: one present when they
/// overlap (their union is at most twice a sprite), else two. Never their
/// bounding box across the screen.
pub(super) fn present_cursor(old: Rect, new: Rect) {
    if !old.intersect(new).is_empty() {
        present(old.union(new));
        return;
    }
    present(old);
    present(new);
}

/// Present `rect` unless it is empty.
pub(super) fn present(rect: Rect) {
    if !rect.is_empty() {
        let _ = sys::display_present(rect.x, rect.y, rect.w, rect.h);
    }
}

/// Present what a sprite move changed outside `damage`, which the caller
/// presents itself: nothing when the sprite stayed put (lifting and
/// stamping at the same place leaves the pixels outside `damage` as they
/// were), else each sprite rectangle not already inside `damage`.
pub(super) fn present_cursor_outside(old: Rect, new: Rect, damage: Rect) {
    if old == new {
        return;
    }
    for rect in [old, new] {
        if rect.intersect(damage) != rect {
            present(rect);
        }
    }
}

/// Boot check of the save-under: stamping and lifting restores the scene
/// byte for byte, a move touches only the two sprite rectangles, and a
/// sprite clipped by the screen edge round-trips too.
/// `XUID:CURSOR:PASS` or `XUID:CURSOR:FAIL <case>`.
pub(super) fn selftest_cursor() -> &'static str {
    const W: i32 = 64;
    const H: i32 = 48;
    let mut pixels = vec![0u8; (W * H * 4) as usize];
    for (index, byte) in pixels.iter_mut().enumerate() {
        *byte = (index * 7 % 251) as u8;
    }
    let scene = pixels.clone();
    // SAFETY: `pixels` is a live `W * H * 4`-byte buffer used only through
    // this canvas until the canvas is dropped at the end of the function.
    let mut screen = unsafe { Canvas::new(pixels.as_mut_ptr() as u64, W, H) };
    let read = |screen: &Canvas| {
        let mut out = vec![0u8; (W * H * 4) as usize];
        screen.save(Rect::new(0, 0, W, H), &mut out);
        out
    };
    let mut overlay = CursorOverlay::new();
    let first = overlay.stamp(&mut screen, (10, 10));
    if first.is_empty() || read(&screen) == scene {
        return "XUID:CURSOR:FAIL stamp drew nothing\n";
    }
    // Move: only the two sprite rectangles may differ from the scene.
    let old = overlay.lift(&mut screen);
    let second = overlay.stamp(&mut screen, (40, 30));
    let now = read(&screen);
    let outside = (0..H).all(|y| {
        (0..W).all(|x| {
            let inside = |r: Rect| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
            let at = ((y * W + x) * 4) as usize;
            inside(second) || now[at..at + 4] == scene[at..at + 4]
        })
    });
    if old != first || !outside {
        return "XUID:CURSOR:FAIL move left a trail\n";
    }
    // Clipped at the bottom-right corner, then lifted: the scene is back.
    overlay.lift(&mut screen);
    let clipped = overlay.stamp(&mut screen, (W - 2, H - 2));
    overlay.lift(&mut screen);
    if clipped.is_empty() || clipped.w >= 11 || read(&screen) != scene {
        return "XUID:CURSOR:FAIL clipped round trip\n";
    }
    "XUID:CURSOR:PASS\n"
}
