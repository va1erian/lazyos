//! What the page changed since the last paint, as WebKit's updates tell it.
//!
//! The classic drawing area of WebKit scrolls its backing store itself and has the web process
//! paint only what the scroll exposed. Each update says so: a rectangle that moved by an offset,
//! then the rectangles that were repainted. The UI process used to hear "the whole view" for any
//! update that scrolled, and copied, converted and handed on every pixel of it for each step of a
//! scroll. [`Pending`] keeps the updates that arrived between two paints in the smallest form the
//! window can apply: one scroll and a few rectangles, or "all of it" when that does not hold.
//!
//! The rules are pure, so they are tested here against a model of the backing store
//! (`xui-webkit`'s `tests/scroll_model.rs`); the C++ only forwards what WebKit says and asks for the [`Plan`].
//!
//! **Superset, never subset.** A rectangle that is repainted needlessly costs a little time; one
//! that is not repainted shows a stale picture. Wherever the exact answer is awkward (a rectangle
//! that straddles the scrolled area, several scrolls in a row) the pending set only grows.

/// A rectangle in view pixels, `x0..x1` by `y0..y1`; empty when either span is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl IRect {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> IRect {
        IRect {
            x0: x,
            y0: y,
            x1: x.saturating_add(width),
            y1: y.saturating_add(height),
        }
    }

    pub fn width(&self) -> i32 {
        (self.x1 - self.x0).max(0)
    }

    pub fn height(&self) -> i32 {
        (self.y1 - self.y0).max(0)
    }

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn area(&self) -> i64 {
        i64::from(self.width()) * i64::from(self.height())
    }

    pub fn intersect(&self, other: &IRect) -> IRect {
        IRect {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        }
    }

    pub fn union(&self, other: &IRect) -> IRect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        IRect {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    pub fn translate(&self, dx: i32, dy: i32) -> IRect {
        IRect {
            x0: self.x0.saturating_add(dx),
            y0: self.y0.saturating_add(dy),
            x1: self.x1.saturating_add(dx),
            y1: self.y1.saturating_add(dy),
        }
    }

    /// Whether `other` lies inside this rectangle (an empty one always does).
    pub fn contains(&self, other: &IRect) -> bool {
        other.is_empty()
            || (self.x0 <= other.x0
                && self.y0 <= other.y0
                && self.x1 >= other.x1
                && self.y1 >= other.y1)
    }

    /// This rectangle without `other`, as up to four disjoint rectangles pushed onto `out`.
    pub fn subtract(&self, other: &IRect, out: &mut Vec<IRect>) {
        let hit = self.intersect(other);
        if self.is_empty() {
            return;
        }
        if hit.is_empty() {
            out.push(*self);
            return;
        }
        let pieces = [
            IRect {
                x0: self.x0,
                y0: self.y0,
                x1: self.x1,
                y1: hit.y0,
            },
            IRect {
                x0: self.x0,
                y0: hit.y1,
                x1: self.x1,
                y1: self.y1,
            },
            IRect {
                x0: self.x0,
                y0: hit.y0,
                x1: hit.x0,
                y1: hit.y1,
            },
            IRect {
                x0: hit.x1,
                y0: hit.y0,
                x1: self.x1,
                y1: hit.y1,
            },
        ];
        out.extend(pieces.into_iter().filter(|piece| !piece.is_empty()));
    }
}

/// The part of the picture that moved: what was inside `area` is now `(dx, dy)` further on, and
/// what the move uncovered is stale until a rectangle repaints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scroll {
    pub area: IRect,
    pub dx: i32,
    pub dy: i32,
}

/// What a paint has to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Paint all of the view and send all of it.
    Full,
    /// Move `scroll` first (if any), then repaint `rects` and send them.
    Delta {
        scroll: Option<Scroll>,
        rects: Vec<IRect>,
    },
}

/// More rectangles than this are replaced by their bounding box: the window applies each as a
/// picture of its own.
pub const MAX_RECTS: usize = 8;

/// When the rectangles add up to this share of the view (in fifths) a whole frame is cheaper.
const FULL_FIFTHS: i64 = 4;

/// The changes waiting for the next paint.
#[derive(Clone, Debug)]
pub struct Pending {
    view: (i32, i32),
    scroll: Option<Scroll>,
    rects: Vec<IRect>,
    full: bool,
    dirty: bool,
}

impl Pending {
    /// Nothing is shown yet: the first paint is a whole frame.
    pub fn new(width: i32, height: i32) -> Pending {
        Pending {
            view: (width.max(0), height.max(0)),
            scroll: None,
            rects: Vec::new(),
            full: true,
            dirty: false,
        }
    }

    fn bounds(&self) -> IRect {
        IRect::new(0, 0, self.view.0, self.view.1)
    }

    /// The view has this size now: nothing painted before it fits.
    pub fn resize(&mut self, width: i32, height: i32) {
        self.view = (width.max(0), height.max(0));
        self.note_full();
    }

    /// Everything must be painted (a load finished, the page's process was replaced).
    pub fn note_full(&mut self) {
        self.full = true;
        self.dirty = true;
        self.scroll = None;
        self.rects.clear();
    }

    /// A rectangle changed with no scroll (the view was reported as needing display).
    pub fn note_rect(&mut self, rect: IRect) {
        self.dirty = true;
        if self.full {
            return;
        }
        self.add(rect);
    }

    /// One update from the page's process, in the order WebKit applied it to its backing store:
    /// `area` moved by `(dx, dy)` (an empty area or a zero offset: nothing moved) and `rects` were
    /// repainted afterwards. An update that names nothing at all is taken as "all of it".
    pub fn note_update(&mut self, area: IRect, dx: i32, dy: i32, rects: &[IRect]) {
        self.dirty = true;
        if self.full {
            return;
        }
        if rects.is_empty() && (area.is_empty() || (dx == 0 && dy == 0)) {
            self.note_full();
            return;
        }
        if !area.is_empty() && (dx != 0 || dy != 0) {
            self.note_scroll(area, dx, dy);
            if self.full {
                return;
            }
        }
        for rect in rects {
            self.add(*rect);
        }
    }

    fn note_scroll(&mut self, area: IRect, dx: i32, dy: i32) {
        let area = area.intersect(&self.bounds());
        if area.is_empty()
            || i64::from(dx.unsigned_abs()) >= i64::from(area.width())
            || i64::from(dy.unsigned_abs()) >= i64::from(area.height())
        {
            // Moved further than it is wide: nothing of it stays in view.
            self.note_full();
            return;
        }
        match &mut self.scroll {
            None => self.scroll = Some(Scroll { area, dx, dy }),
            Some(scroll) if scroll.area == area => {
                scroll.dx = scroll.dx.saturating_add(dx);
                scroll.dy = scroll.dy.saturating_add(dy);
            }
            // A second scrolled area: they cannot be told as one move.
            Some(_) => {
                self.note_full();
                return;
            }
        }
        // What is waiting to be repainted moves with the picture. Both the old and the new place
        // are kept: the old one is stale or about to be uncovered, the new one is where it went.
        let moved: Vec<IRect> = self
            .rects
            .iter()
            .map(|rect| rect.intersect(&area).translate(dx, dy).intersect(&area))
            .filter(|rect| !rect.is_empty())
            .collect();
        for rect in moved {
            self.add(rect);
        }
        if let Some(scroll) = &self.scroll {
            if i64::from(scroll.dx.unsigned_abs()) >= i64::from(area.width())
                || i64::from(scroll.dy.unsigned_abs()) >= i64::from(area.height())
            {
                self.note_full();
            } else if scroll.dx == 0 && scroll.dy == 0 {
                self.scroll = None;
            }
        }
    }

    /// Adds a rectangle, clipped to the view, merging what it covers or is covered by.
    fn add(&mut self, rect: IRect) {
        let rect = rect.intersect(&self.bounds());
        if rect.is_empty() || self.rects.iter().any(|have| have.contains(&rect)) {
            return;
        }
        self.rects.retain(|have| !rect.contains(have));
        self.rects.push(rect);
        if self.rects.len() > MAX_RECTS {
            let all = self
                .rects
                .iter()
                .fold(IRect::default(), |sum, r| sum.union(r));
            self.rects.clear();
            self.rects.push(all);
        }
        let covered: i64 = self.rects.iter().map(IRect::area).sum();
        if covered * 5 >= self.bounds().area() * FULL_FIFTHS {
            self.note_full();
        }
    }

    /// What the next paint has to do, and forget it; `None` when nothing changed.
    pub fn take_plan(&mut self) -> Option<Plan> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        if std::mem::take(&mut self.full) {
            self.scroll = None;
            self.rects.clear();
            return Some(Plan::Full);
        }
        let scroll = self.scroll.take();
        let rects = std::mem::take(&mut self.rects);
        if scroll.is_none() && rects.is_empty() {
            return None;
        }
        Some(Plan::Delta { scroll, rects })
    }
}

#[cfg(test)]
mod tests;
