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
use core::sync::atomic::{AtomicI32, Ordering};
use cursorgfx::{Shape, Sprite, BUSY_FRAMES, SHAPE_COUNT};
use user::messenger::display::{Canvas, Rect};
use user::sys;

use super::compositor::Compositor;

/// What a stamped sprite looks like: the shape and, for the busy spinner,
/// its rotation frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Look {
    pub(super) shape: Shape,
    pub(super) frame: u32,
}

impl Look {
    const DEFAULT: Look = Look {
        shape: Shape::Default,
        frame: 0,
    };

    /// The sprite cache slot: one per shape, plus the busy frames.
    fn slot(self) -> usize {
        match self.shape {
            Shape::Busy => SHAPE_COUNT + (self.frame % BUSY_FRAMES) as usize,
            shape => shape as usize,
        }
    }
}

/// Where the sprite is on screen and the scene it hides.
struct Shown {
    /// The rectangle the sprite was stamped into (clipped to the screen).
    rect: Rect,
    /// The pointer it was drawn for.
    at: (i32, i32),
    look: Look,
}

/// Side of the largest sprite for the current size: how far any sprite
/// reaches from the pointer (drag damage uses it, `layout::cursor_rect`).
static EXTENT: AtomicI32 = AtomicI32::new(0);

/// The most the sprite can reach from its hotspot, in pixels.
pub(super) fn extent() -> i32 {
    EXTENT.load(Ordering::Relaxed)
}

pub(super) struct CursorOverlay {
    shown: Option<Shown>,
    /// What the next stamp draws.
    look: Look,
    /// The cursor's design size (`sys/ui/cursor/size`); it is drawn at this
    /// times the UI scale physical pixels.
    design: u32,
    /// The scene pixels under `shown`, row after row, sized for the largest
    /// sprite of the current size.
    saved: Vec<u8>,
    /// Rendered sprites by [`Look::slot`], dropped when the size changes.
    cache: Vec<Option<Sprite>>,
}

impl CursorOverlay {
    pub(super) fn new() -> CursorOverlay {
        let mut overlay = CursorOverlay {
            shown: None,
            look: Look::DEFAULT,
            design: uitheme::DEFAULT_CURSOR_SIZE,
            saved: Vec::new(),
            cache: Vec::new(),
        };
        overlay.resize();
        overlay
    }

    /// Physical pixels of the sprite's design square.
    fn px(&self) -> u32 {
        self.design * super::theme::scale() as u32
    }

    /// Rebuild the cache and save-under buffer for the current size.
    fn resize(&mut self) {
        let extent = cursorgfx::max_extent(self.px()) as usize;
        self.saved = vec![0; extent * extent * 4];
        self.cache = (0..SHAPE_COUNT + BUSY_FRAMES as usize)
            .map(|_| None)
            .collect();
        EXTENT.store(extent as i32, Ordering::Relaxed);
    }

    /// The design size in effect.
    pub(super) fn design_size(&self) -> u32 {
        self.design
    }

    /// Change the size. Call only while the sprite is lifted: the save-under
    /// buffer is replaced.
    pub(super) fn set_design_size(&mut self, design: u32) {
        if design != self.design {
            self.design = design;
            self.resize();
        }
    }

    /// Choose what the next stamp draws; `true` when it changed.
    pub(super) fn set_look(&mut self, look: Look) -> bool {
        let changed = look != self.look;
        self.look = look;
        changed
    }

    /// Put the scene back where the sprite is, so the screen buffer holds
    /// the pure scene; returns the rectangle that changed (empty when the
    /// sprite was not shown).
    pub(super) fn lift(&mut self, screen: &mut Canvas) -> Rect {
        match self.shown.take() {
            Some(shown) => {
                screen.restore(shown.rect, &self.saved);
                shown.rect
            }
            None => Rect::default(),
        }
    }

    /// Save the scene under the sprite at `at` and draw it there; returns the
    /// rectangle that changed.
    pub(super) fn stamp(&mut self, screen: &mut Canvas, at: (i32, i32)) -> Rect {
        let (look, px) = (self.look, self.px());
        let sprite = self.cache[look.slot()]
            .get_or_insert_with(|| cursorgfx::render(look.shape, px, look.frame));
        let rect = Rect::new(
            at.0.saturating_sub(sprite.hot_x as i32),
            at.1.saturating_sub(sprite.hot_y as i32),
            sprite.w as i32,
            sprite.h as i32,
        );
        let saved = screen.save(rect, &mut self.saved);
        if saved.is_empty() {
            return saved;
        }
        screen.blit_premultiplied(rect.x, rect.y, sprite.w as i32, &sprite.pixels, saved);
        self.shown = Some(Shown {
            rect: saved,
            at,
            look,
        });
        saved
    }

    /// The look the next stamp draws.
    pub(super) fn look(&self) -> Look {
        self.look
    }

    /// Whether what is stamped differs from `target` drawn with the current
    /// look: the sprite must be redrawn.
    fn stale(&self, target: Option<(i32, i32)>) -> bool {
        let drawn = self.shown.as_ref().map(|shown| (shown.at, shown.look));
        drawn != target.map(|at| (at, self.look))
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
        self.cursor.set_look(self.cursor_look());
        if !self.cursor.stale(target) {
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
/// byte for byte, a move touches only the two sprite rectangles, a sprite
/// clipped by the screen edge round-trips too, and the stamped rectangle is
/// the sprite's, hotspot included, for the arrow and the hand.
/// `XUID:CURSOR:PASS` or `XUID:CURSOR:FAIL <case>`.
pub(super) fn selftest_cursor() -> &'static str {
    const W: i32 = 96;
    const H: i32 = 80;
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
    let px = overlay.px();
    let first = overlay.stamp(&mut screen, (20, 20));
    let arrow = cursorgfx::render(Shape::Default, px, 0);
    let want = Rect::new(
        20 - arrow.hot_x as i32,
        20 - arrow.hot_y as i32,
        arrow.w as i32,
        arrow.h as i32,
    );
    if first.is_empty() || read(&screen) == scene {
        return "XUID:CURSOR:FAIL stamp drew nothing
";
    }
    if first != want {
        return "XUID:CURSOR:FAIL stamp rect is not the sprite's
";
    }
    // Move: only the two sprite rectangles may differ from the scene.
    let old = overlay.lift(&mut screen);
    let second = overlay.stamp(&mut screen, (60, 40));
    let now = read(&screen);
    let outside = (0..H).all(|y| {
        (0..W).all(|x| {
            let inside = |r: Rect| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
            let at = ((y * W + x) * 4) as usize;
            inside(second) || now[at..at + 4] == scene[at..at + 4]
        })
    });
    if old != first || !outside {
        return "XUID:CURSOR:FAIL move left a trail
";
    }
    // Resize shapes come from the frame edges the pointer is on.
    let frame = Rect::new(30, 30, 300, 200);
    let at = |x: i32, y: i32| super::geometry::hit_edges(frame, (x, y)).cursor_shape();
    if at(30, 100) != Shape::ResizeH
        || at(100, 30) != Shape::ResizeV
        || at(31, 31) != Shape::ResizeNwse
        || at(329, 229) != Shape::ResizeNwse
        || at(31, 229) != Shape::ResizeNesw
        || at(100, 100) != Shape::Default
    {
        return "XUID:CURSOR:FAIL resize shapes
";
    }
    // The hand: another hotspot, same exact restore.
    overlay.lift(&mut screen);
    overlay.set_look(Look {
        shape: Shape::Hand,
        frame: 0,
    });
    let hand = cursorgfx::render(Shape::Hand, px, 0);
    let third = overlay.stamp(&mut screen, (40, 40));
    if third.x != 40 - hand.hot_x as i32 || third.y != 40 - hand.hot_y as i32 {
        return "XUID:CURSOR:FAIL hand hotspot
";
    }
    overlay.lift(&mut screen);
    if read(&screen) != scene {
        return "XUID:CURSOR:FAIL hand lift
";
    }
    // Clipped at the bottom-right corner, then lifted: the scene is back.
    let clipped = overlay.stamp(&mut screen, (W - 2, H - 2));
    overlay.lift(&mut screen);
    if clipped.is_empty() || clipped.w >= hand.w as i32 || read(&screen) != scene {
        return "XUID:CURSOR:FAIL clipped round trip
";
    }
    "XUID:CURSOR:PASS
"
}
