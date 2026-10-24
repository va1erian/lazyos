//! The painter that writes the picture straight into the window's frame on LazyOS (`raster.rs`, the
//! backend's `direct.rs`): one copy of each row instead of the canvas's passes. The canvas painter
//! in `view.rs` draws the same picture for the frames something covers the view (and on other
//! platforms).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use xui_app::backend::direct::{set_painter, Target};
use xui_core::backend::WidgetId;
use xui_core::geometry::Rect;

use crate::damage::IRect;
use crate::raster::{raster, Pixels};
use crate::view::State;

fn irect(rect: Rect) -> IRect {
    IRect {
        x0: rect.left,
        y0: rect.top,
        x1: rect.right,
        y1: rect.bottom,
    }
}

pub(crate) fn install(id: WidgetId, state: &Rc<RefCell<State>>) {
    let state = Rc::clone(state);
    set_painter(
        id,
        Some(Rc::new(move |target: &mut Target<'_>| {
            let Ok(state) = state.try_borrow() else {
                return false;
            };
            let started = Instant::now();
            let (ahead, offset) = state.shown();
            let view = irect(target.bounds);
            let mut pixels = Pixels {
                data: target.pixels,
                stride: target.stride,
            };
            let checker = raster(
                &mut pixels,
                view,
                irect(target.damage),
                &state.picture,
                &state.bands.borrow(),
                ahead,
                offset,
            );
            state.record_draw(started, view.height().max(0) as u32, checker, ahead);
            true
        })),
    );
}
