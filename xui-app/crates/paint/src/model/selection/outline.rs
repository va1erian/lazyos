#![forbid(unsafe_code)]

//! The outline of a selection as grid-line segments, for the marching ants.

use super::Selection;

/// The most segments an outline holds. A noisy wand over a photograph can have
/// an edge per pixel; past this the outline is cut short (the ants stop
/// helping long before) instead of costing a frame per redraw.
pub const MAX_EDGES: usize = 200_000;

/// One straight run of the boundary between selected and unselected pixels,
/// on the pixel grid: `(x0, y0)` to `(x1, y1)`, horizontal (`y0 == y1`) or
/// vertical (`x0 == x1`), `x0 <= x1` and `y0 <= y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    /// Start x, in grid lines.
    pub x0: i32,
    /// Start y.
    pub y0: i32,
    /// End x.
    pub x1: i32,
    /// End y.
    pub y1: i32,
}

impl Edge {
    /// Whether the run is horizontal.
    pub fn horizontal(&self) -> bool {
        self.y0 == self.y1
    }
}

impl Selection {
    /// The boundary, merged into the longest straight runs. A rectangle is
    /// four edges. Pixels at least half covered count as inside.
    pub fn edges(&self) -> Vec<Edge> {
        let b = self.bounds;
        if self.mask.is_none() {
            return vec![
                Edge {
                    x0: b.x0,
                    y0: b.y0,
                    x1: b.x1,
                    y1: b.y0,
                },
                Edge {
                    x0: b.x1,
                    y0: b.y0,
                    x1: b.x1,
                    y1: b.y1,
                },
                Edge {
                    x0: b.x0,
                    y0: b.y1,
                    x1: b.x1,
                    y1: b.y1,
                },
                Edge {
                    x0: b.x0,
                    y0: b.y0,
                    x1: b.x0,
                    y1: b.y1,
                },
            ];
        }
        let inside = |x: i32, y: i32| self.contains(x, y);
        let mut edges = Vec::new();
        // Horizontal edges lie on the line y between rows y - 1 and y.
        for y in b.y0..=b.y1 {
            let mut start: Option<i32> = None;
            for x in b.x0..=b.x1 {
                let edge = x < b.x1 && inside(x, y - 1) != inside(x, y);
                match (edge, start) {
                    (true, None) => start = Some(x),
                    (false, Some(from)) => {
                        edges.push(Edge {
                            x0: from,
                            y0: y,
                            x1: x,
                            y1: y,
                        });
                        start = None;
                    }
                    _ => {}
                }
            }
            if edges.len() >= MAX_EDGES {
                return edges;
            }
        }
        // Vertical edges lie on the line x between columns x - 1 and x.
        for x in b.x0..=b.x1 {
            let mut start: Option<i32> = None;
            for y in b.y0..=b.y1 {
                let edge = y < b.y1 && inside(x - 1, y) != inside(x, y);
                match (edge, start) {
                    (true, None) => start = Some(y),
                    (false, Some(from)) => {
                        edges.push(Edge {
                            x0: x,
                            y0: from,
                            x1: x,
                            y1: y,
                        });
                        start = None;
                    }
                    _ => {}
                }
            }
            if edges.len() >= MAX_EDGES {
                break;
            }
        }
        edges
    }
}
