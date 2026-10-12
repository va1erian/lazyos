#![forbid(unsafe_code)]

//! An integer rectangle in pixel coordinates, half-open: `x0..x1` by `y0..y1`.
//!
//! History entries, dirty regions and the flattened cache all speak in these.
//! Arithmetic saturates, so a rectangle built from hostile coordinates is still
//! a valid (if huge) rectangle that [`Rect::intersect`] then clips.

/// A half-open pixel rectangle. Empty when `x1 <= x0` or `y1 <= y0`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    /// Left edge, inclusive.
    pub x0: i32,
    /// Top edge, inclusive.
    pub y0: i32,
    /// Right edge, exclusive.
    pub x1: i32,
    /// Bottom edge, exclusive.
    pub y1: i32,
}

impl Rect {
    /// The empty rectangle; the identity of [`Rect::union`].
    pub const EMPTY: Rect = Rect {
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 0,
    };

    /// A rectangle at `(x, y)` of `width` x `height`.
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect {
            x0: x,
            y0: y,
            x1: x.saturating_add_unsigned(width),
            y1: y.saturating_add_unsigned(height),
        }
    }

    /// The smallest rectangle holding both points (inclusive pixels).
    pub fn from_points(a: (i32, i32), b: (i32, i32)) -> Rect {
        Rect {
            x0: a.0.min(b.0),
            y0: a.1.min(b.1),
            x1: a.0.max(b.0).saturating_add(1),
            y1: a.1.max(b.1).saturating_add(1),
        }
    }

    /// The width, zero when empty.
    pub fn width(&self) -> u32 {
        self.x1.saturating_sub(self.x0).max(0) as u32
    }

    /// The height, zero when empty.
    pub fn height(&self) -> u32 {
        self.y1.saturating_sub(self.y0).max(0) as u32
    }

    /// The pixel count.
    pub fn area(&self) -> usize {
        self.width() as usize * self.height() as usize
    }

    /// Whether the rectangle holds no pixel.
    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    /// Whether `(x, y)` is inside.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    /// The overlap of two rectangles (empty when they miss).
    pub fn intersect(&self, other: Rect) -> Rect {
        let r = Rect {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        if r.is_empty() { Rect::EMPTY } else { r }
    }

    /// The smallest rectangle holding both; an empty one contributes nothing.
    pub fn union(&self, other: Rect) -> Rect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return *self;
        }
        Rect {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// Grown by `n` pixels on every side.
    pub fn inflate(&self, n: i32) -> Rect {
        Rect {
            x0: self.x0.saturating_sub(n),
            y0: self.y0.saturating_sub(n),
            x1: self.x1.saturating_add(n),
            y1: self.y1.saturating_add(n),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersect_and_union() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(b), Rect::new(5, 5, 5, 5));
        assert_eq!(a.union(b), Rect::new(0, 0, 15, 15));
        assert!(a.intersect(Rect::new(20, 0, 5, 5)).is_empty());
        assert_eq!(Rect::EMPTY.union(a), a);
        assert_eq!(a.union(Rect::EMPTY), a);
    }

    #[test]
    fn extreme_coordinates_saturate() {
        let r = Rect::from_points((i32::MIN, i32::MIN), (i32::MAX, i32::MAX));
        assert!(!r.is_empty());
        assert_eq!(r.intersect(Rect::new(0, 0, 4, 4)), Rect::new(0, 0, 4, 4));
        let _ = r.inflate(i32::MAX).width();
    }
}
