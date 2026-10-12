#![forbid(unsafe_code)]

//! What part of the picture changed since the view last looked.

use super::rect::Rect;

/// The region of the flattened picture a mutation touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Damage {
    /// Nothing changed.
    #[default]
    None,
    /// Only this rectangle (document pixels) changed.
    Rect(Rect),
    /// Everything: a resize, a load, a layer reorder or a visibility change.
    All,
}

impl Damage {
    /// The combination of two damages.
    pub fn merge(self, other: Damage) -> Damage {
        match (self, other) {
            (Damage::All, _) | (_, Damage::All) => Damage::All,
            (Damage::None, d) | (d, Damage::None) => d,
            (Damage::Rect(a), Damage::Rect(b)) => Damage::Rect(a.union(b)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_unions_rects_and_all_wins() {
        let a = Damage::Rect(Rect::new(0, 0, 2, 2));
        let b = Damage::Rect(Rect::new(4, 4, 2, 2));
        assert_eq!(a.merge(b), Damage::Rect(Rect::new(0, 0, 6, 6)));
        assert_eq!(a.merge(Damage::All), Damage::All);
        assert_eq!(Damage::None.merge(a), a);
        assert_eq!(Damage::None.merge(Damage::None), Damage::None);
    }
}
