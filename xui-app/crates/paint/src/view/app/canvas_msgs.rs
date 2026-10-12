#![forbid(unsafe_code)]

//! What canvas input means: the model's tools for most, the view's own for
//! Zoom, and text typing.

use super::PaintApp;
use crate::model::{Side, Tool};
use crate::view::CanvasMsg;

/// A Zoom drag shorter than this many screen pixels is a click.
const CLICK_SLOP_PX: f64 = 4.0;

/// A Zoom tool drag in progress.
#[derive(Clone, Copy)]
pub(super) struct ZoomDrag {
    start: (i32, i32),
    side: Side,
}

impl PaintApp {
    /// Handles one canvas message.
    pub(super) fn canvas_msg(&mut self, msg: CanvasMsg) {
        match msg {
            CanvasMsg::Down { x, y, side, mods } => {
                if self.model.is_dragging() {
                    self.model.end();
                }
                self.hint = None;
                self.cursor = Some((x, y));
                if self.model.tool() == Tool::Zoom {
                    self.zoom_drag = Some(ZoomDrag { start: (x, y), side });
                } else {
                    self.model.begin_with(x, y, side, mods);
                }
            }
            CanvasMsg::Move { x, y, mods } => {
                // The pointer is on the canvas, so it is no longer on the grid
                // (a backend need not send the grid a leave).
                self.hint = None;
                self.cursor = Some((x, y));
                if let Some(drag) = self.zoom_drag {
                    let band = self.is_zoom_band(drag.start, (x, y));
                    let marquee = (band && drag.side == Side::Primary).then_some((drag.start, (x, y)));
                    self.canvas.set_marquee(marquee);
                } else {
                    self.model.extend_with(x, y, mods);
                }
            }
            CanvasMsg::Up { x, y, mods } => {
                self.cursor = Some((x, y));
                if let Some(drag) = self.zoom_drag.take() {
                    self.finish_zoom(drag, (x, y));
                } else {
                    self.model.extend_with(x, y, mods);
                    self.model.end();
                }
            }
            // Capture or focus lost mid-drag: keep what was drawn.
            CanvasMsg::Cancel => {
                self.model.end();
                self.end_zoom_drag();
                self.cursor = None;
            }
            // Esc: take the stroke back.
            CanvasMsg::Abort => {
                self.model.cancel();
                self.end_zoom_drag();
            }
            CanvasMsg::Char(c) => self.model.type_char(c),
            CanvasMsg::Text(key) => self.model.text_key(key),
            // The canvas zoomed or scrolled itself; the sync after every
            // message refreshes the zoom part of the status bar.
            CanvasMsg::View => {}
        }
    }

    fn end_zoom_drag(&mut self) {
        self.zoom_drag = None;
        self.canvas.set_marquee(None);
    }

    /// Whether a drag from `a` to `b` is wider than a click.
    fn is_zoom_band(&self, a: (i32, i32), b: (i32, i32)) -> bool {
        let zoom = self.canvas.viewport().zoom();
        let span = f64::from((a.0 - b.0).abs().max((a.1 - b.1).abs()));
        span * zoom > CLICK_SLOP_PX
    }

    /// The Zoom tool's release: a click zooms in (the right button out), a
    /// left drag zooms to its rectangle.
    fn finish_zoom(&mut self, drag: ZoomDrag, end: (i32, i32)) {
        self.canvas.set_marquee(None);
        match (self.is_zoom_band(drag.start, end), drag.side) {
            (true, Side::Primary) => self.canvas.zoom_to_rect(drag.start, end),
            (_, Side::Primary) => self.canvas.zoom_in_at(drag.start),
            (_, Side::Secondary) => self.canvas.zoom_out_at(drag.start),
        }
    }
}
