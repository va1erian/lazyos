//! Unit tests for the offscreen backend: per-node clipping, hit-testing and
//! layout. Pointer capture has its own file, [`super::capture_tests`].

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::app::{App, Ui, run_app};
use xui_core::backend::{
    Backend, Event, NodeKind, NodeSpec, Painter, ParentRef, PlatformSpec, WidgetId,
};
use xui_core::message::{Modifiers, MouseButton};
use xui_core::widget::{Label, ScrollView};
use xui_core::{Color, Dip, Rect, Theme};

use super::OffscreenBackend;
use crate::tests::save;

mod capture;
mod clipboard;
mod hit;
mod paint;
mod window_icon;

/// A sink that records every event it is handed.
pub(super) struct Recorder(pub(super) Rc<RefCell<Vec<(WidgetId, Event)>>>);

impl xui_core::router::WidgetHost for Recorder {
    fn deliver(&self, target: WidgetId, event: &Event) -> bool {
        self.0.borrow_mut().push((target, *event));
        true
    }
}

fn red_pixels(image: &crate::RgbaImage) -> usize {
    image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[0] > 200 && pixel[1] < 80 && pixel[2] < 80)
        .count()
}

fn blue_pixels(image: &crate::RgbaImage) -> usize {
    image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[0] < 80 && pixel[1] < 80 && pixel[2] > 200)
        .count()
}

/// A painter that fills the node's bounds with a solid colour.
fn fill(color: Color) -> Painter {
    Rc::new(move |canvas| {
        let bounds = canvas.bounds();
        canvas.fill_rect(bounds, color);
    })
}
