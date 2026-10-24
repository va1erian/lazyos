//! The editing model of Cursor Studio: one theme, the shape being drawn, the
//! selection, a pending polygon and an undo history. No UI, no I/O.
//!
//! Points are design units (0..=512 over the 32x32 design square), the unit
//! `cursorgfx` and the `.lcur` format use. Everything a user can do is a
//! method here so it is host tested; the canvas and buttons only call them.

use cursorgfx::theme::{DESIGN, MAX_LAYERS, MAX_VERTICES};
use cursorgfx::{LayerDef, Shape, ShapeDef, Theme};

/// Snap step in design units: half a grid cell, the built-in shapes' grain.
pub const SNAP: i32 = 8;
/// Undo steps kept.
const HISTORY: usize = 200;

/// What a click on the canvas does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tool {
    /// Pick and drag vertices.
    #[default]
    Select,
    /// Click an edge to add a vertex there.
    AddVertex,
    /// Click points to outline a new polygon.
    NewPolygon,
    /// Click to place the pointer position.
    Hotspot,
}

/// A vertex: layer, polygon in the layer, vertex in the polygon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pick {
    pub layer: usize,
    pub poly: usize,
    pub vertex: usize,
}

pub struct Model {
    pub theme: Theme,
    pub shape: Shape,
    pub tool: Tool,
    pub snap: bool,
    pub selected: Option<Pick>,
    /// The layer new polygons go to.
    pub layer: usize,
    /// The polygon being outlined by [`Tool::NewPolygon`].
    pub pending: Vec<(i32, i32)>,
    undo: Vec<Theme>,
    redo: Vec<Theme>,
    /// The theme last applied (or loaded), to tell unapplied edits.
    applied: Theme,
}

fn light_layer() -> LayerDef {
    LayerDef {
        polys: Vec::new(),
        fill: 0xf0f0f0,
        outline: 0x000000,
        thin: false,
    }
}

impl Model {
    /// Start editing `theme`.
    pub fn new(theme: Theme) -> Model {
        Model {
            applied: theme.clone(),
            theme,
            shape: Shape::Default,
            tool: Tool::Select,
            snap: true,
            selected: None,
            layer: 0,
            pending: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// A theme of the built-in shapes, the starting point of a new one.
    pub fn builtin() -> Model {
        Model::new(Theme::builtin("mine"))
    }

    /// The definition being edited, `None` while the shape is the built-in.
    pub fn def(&self) -> Option<&ShapeDef> {
        self.theme.shapes[self.shape as usize].as_ref()
    }

    fn def_mut(&mut self) -> Option<&mut ShapeDef> {
        self.theme.shapes[self.shape as usize].as_mut()
    }

    /// Whether the theme differs from what was last applied.
    pub fn dirty(&self) -> bool {
        self.theme != self.applied
    }

    /// Record `theme` as applied (after a successful write).
    pub fn mark_applied(&mut self) {
        self.applied = self.theme.clone();
    }

    /// Replace the whole theme (opening a file); the history starts over.
    pub fn load(&mut self, theme: Theme) {
        self.applied = theme.clone();
        self.theme = theme;
        self.undo.clear();
        self.redo.clear();
        self.clear_selection();
    }

    fn clear_selection(&mut self) {
        self.selected = None;
        self.pending.clear();
        self.layer = 0;
    }

    /// Edit another shape.
    pub fn pick_shape(&mut self, shape: Shape) {
        self.shape = shape;
        self.clear_selection();
    }

    /// Save the theme to the undo history before a change.
    pub fn checkpoint(&mut self) {
        if self.undo.len() == HISTORY {
            self.undo.remove(0);
        }
        self.undo.push(self.theme.clone());
        self.redo.clear();
    }

    /// After undo or redo: the vertex pick is stale, but the active layer
    /// stays (clamped to what exists) and so does a half-drawn polygon.
    fn after_history(&mut self) {
        self.selected = None;
        let layers = self.def().map_or(0, |d| d.layers.len());
        self.layer = self.layer.min(layers.saturating_sub(1));
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo.push(std::mem::replace(&mut self.theme, previous));
        self.after_history();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(std::mem::replace(&mut self.theme, next));
        self.after_history();
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Make the current shape editable: copy the built-in drawing into the
    /// theme (Busy, which has no static drawing, starts from the arrow).
    pub fn customize(&mut self) {
        if self.def().is_some() {
            return;
        }
        self.checkpoint();
        let from = if self.shape == Shape::Busy {
            Shape::Default
        } else {
            self.shape
        };
        let def = Theme::builtin("").shapes[from as usize].clone();
        self.theme.shapes[self.shape as usize] = def;
        self.clear_selection();
    }

    /// Give the shape back to the built-in drawing.
    pub fn reset_shape(&mut self) {
        if self.def().is_some() {
            self.checkpoint();
            self.theme.shapes[self.shape as usize] = None;
            self.clear_selection();
        }
    }

    /// `point` clamped into the design square and snapped when snapping.
    pub fn snapped(&self, (x, y): (i32, i32)) -> (i32, i32) {
        let fit = |v: i32| {
            let v = v.clamp(0, DESIGN);
            if self.snap {
                ((v + SNAP / 2) / SNAP * SNAP).min(DESIGN)
            } else {
                v
            }
        };
        (fit(x), fit(y))
    }

    /// The vertex within `radius` of `point` (the closest one, topmost layer
    /// first on a tie).
    pub fn hit_vertex(&self, point: (i32, i32), radius: i32) -> Option<Pick> {
        let def = self.def()?;
        let mut best: Option<(i64, Pick)> = None;
        for (layer, l) in def.layers.iter().enumerate().rev() {
            for (poly, p) in l.polys.iter().enumerate() {
                for (vertex, &(x, y)) in p.iter().enumerate() {
                    let d = i64::from(x - point.0).pow(2) + i64::from(y - point.1).pow(2);
                    if d <= i64::from(radius).pow(2) && best.is_none_or(|(b, _)| d < b) {
                        best = Some((
                            d,
                            Pick {
                                layer,
                                poly,
                                vertex,
                            },
                        ));
                    }
                }
            }
        }
        best.map(|(_, pick)| pick)
    }

    /// Move the selected vertex to `point` (no checkpoint: the drag took it).
    pub fn move_selected(&mut self, point: (i32, i32)) {
        let to = self.snapped(point);
        let Some(pick) = self.selected else { return };
        if let Some(v) = self
            .def_mut()
            .and_then(|d| d.layers.get_mut(pick.layer))
            .and_then(|l| l.polys.get_mut(pick.poly))
            .and_then(|p| p.get_mut(pick.vertex))
        {
            *v = to;
        }
    }

    /// Insert a vertex on the edge of the active layer nearest `point`, within
    /// `radius`; the new vertex becomes the selection.
    pub fn insert_vertex(&mut self, point: (i32, i32), radius: i32) -> bool {
        let to = self.snapped(point);
        let Some(def) = self.def() else { return false };
        let mut best: Option<(i64, usize, usize, usize)> = None;
        for (layer, l) in def.layers.iter().enumerate() {
            for (poly, p) in l.polys.iter().enumerate() {
                for i in 0..p.len() {
                    let d = segment_distance2(point, p[i], p[(i + 1) % p.len()]);
                    if d <= i64::from(radius).pow(2) && best.is_none_or(|(b, ..)| d < b) {
                        best = Some((d, layer, poly, i + 1));
                    }
                }
            }
        }
        let Some((_, layer, poly, at)) = best else {
            return false;
        };
        if self.vertex_count() >= MAX_VERTICES {
            return false;
        }
        self.checkpoint();
        if let Some(p) = self
            .def_mut()
            .and_then(|d| d.layers.get_mut(layer))
            .and_then(|l| l.polys.get_mut(poly))
        {
            p.insert(at, to);
            self.selected = Some(Pick {
                layer,
                poly,
                vertex: at,
            });
        }
        true
    }

    fn vertex_count(&self) -> usize {
        self.def().map_or(0, |d| {
            d.layers.iter().flat_map(|l| &l.polys).map(Vec::len).sum()
        })
    }

    /// Delete the selected vertex; a polygon left with two points goes, then
    /// an empty layer, then the whole drawing (back to the built-in shape).
    pub fn delete_selected(&mut self) -> bool {
        let Some(pick) = self.selected else {
            return false;
        };
        if self.def().is_none() {
            return false;
        }
        self.checkpoint();
        self.selected = None;
        let Some(def) = self.def_mut() else {
            return false;
        };
        let layer = &mut def.layers[pick.layer];
        layer.polys[pick.poly].remove(pick.vertex);
        if layer.polys[pick.poly].len() < 3 {
            layer.polys.remove(pick.poly);
        }
        if layer.polys.is_empty() {
            def.layers.remove(pick.layer);
        }
        if def.layers.iter().all(|l| l.polys.is_empty()) {
            self.theme.shapes[self.shape as usize] = None;
        }
        self.layer = 0;
        true
    }

    /// Add a point of the polygon being outlined.
    pub fn add_pending(&mut self, point: (i32, i32)) {
        if self.vertex_count() + self.pending.len() < MAX_VERTICES {
            let to = self.snapped(point);
            self.pending.push(to);
        }
    }

    /// Close the pending polygon into the active layer; `false` while it has
    /// fewer than three points (they are kept).
    pub fn finish_polygon(&mut self) -> bool {
        if self.pending.len() < 3 {
            return false;
        }
        self.checkpoint();
        if self.def().is_none() {
            self.theme.shapes[self.shape as usize] = Some(ShapeDef {
                hot: (0, 0),
                layers: Vec::new(),
            });
        }
        let points = std::mem::take(&mut self.pending);
        let layer = self.layer;
        let Some(def) = self.def_mut() else {
            return false;
        };
        if def.layers.is_empty() {
            def.layers.push(light_layer());
        }
        let at = layer.min(def.layers.len() - 1);
        def.layers[at].polys.push(points);
        self.layer = at;
        true
    }

    pub fn cancel_pending(&mut self) {
        self.pending.clear();
    }

    /// Place the pointer position.
    pub fn set_hotspot(&mut self, point: (i32, i32)) {
        let to = self.snapped(point);
        if self.def().is_some_and(|d| d.hot != to) {
            self.checkpoint();
            if let Some(def) = self.def_mut() {
                def.hot = to;
            }
        }
    }

    /// Add an empty layer after the last one and make it active.
    pub fn add_layer(&mut self) -> bool {
        if self.def().is_none_or(|d| d.layers.len() >= MAX_LAYERS) {
            return false;
        }
        self.checkpoint();
        let mut last = 0;
        if let Some(def) = self.def_mut() {
            def.layers.push(light_layer());
            last = def.layers.len() - 1;
        }
        self.layer = last;
        true
    }

    /// Set the active layer's colours; `false` when there is no such layer.
    pub fn set_colours(&mut self, fill: u32, outline: u32) -> bool {
        let layer = self.layer;
        if self.def().is_none_or(|d| layer >= d.layers.len()) {
            return false;
        }
        let (fill, outline) = (fill & 0xff_ffff, outline & 0xff_ffff);
        if self
            .active_layer()
            .is_some_and(|l| (l.fill, l.outline) == (fill, outline))
        {
            return true;
        }
        self.checkpoint();
        if let Some(l) = self.def_mut().and_then(|d| d.layers.get_mut(layer)) {
            l.fill = fill;
            l.outline = outline;
        }
        true
    }

    pub fn toggle_thin(&mut self) -> bool {
        let layer = self.layer;
        if self.def().is_none_or(|d| layer >= d.layers.len()) {
            return false;
        }
        self.checkpoint();
        if let Some(l) = self.def_mut().and_then(|d| d.layers.get_mut(layer)) {
            l.thin = !l.thin;
        }
        true
    }

    /// The active layer, when the shape has one.
    pub fn active_layer(&self) -> Option<&LayerDef> {
        self.def().and_then(|d| d.layers.get(self.layer))
    }
}

/// Squared distance from `p` to the segment `a`-`b`.
fn segment_distance2(p: (i32, i32), a: (i32, i32), b: (i32, i32)) -> i64 {
    let (px, py) = (i64::from(p.0), i64::from(p.1));
    let (ax, ay) = (i64::from(a.0), i64::from(a.1));
    let (dx, dy) = (i64::from(b.0) - ax, i64::from(b.1) - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0 {
        0.0
    } else {
        (((px - ax) * dx + (py - ay) * dy) as f64 / len2 as f64).clamp(0.0, 1.0)
    };
    let (qx, qy) = (ax as f64 + dx as f64 * t, ay as f64 + dy as f64 * t);
    ((px as f64 - qx).powi(2) + (py as f64 - qy).powi(2)) as i64
}

#[cfg(test)]
mod tests;
