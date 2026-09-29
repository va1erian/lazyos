#![forbid(unsafe_code)]

//! Event coordinate translation for the offscreen backend. The node geometry
//! traversal lives in [`crate::geometry`].

use xui_core::backend::{Event, ParentRef};
use xui_core::geometry::Rect;

use super::Node;
use crate::geometry::GeometryNode;

impl GeometryNode for Node {
    fn parent(&self) -> ParentRef {
        self.parent
    }

    fn bounds(&self) -> Rect {
        self.bounds
    }

    fn clip(&self) -> Option<Rect> {
        self.clip
    }

    fn visible(&self) -> bool {
        self.visible
    }

    fn enabled(&self) -> bool {
        self.enabled
    }
}

/// Rebuilds `event` with its cursor position at `(x, y)`.
pub(super) fn translate(event: Event, x: i32, y: i32) -> Event {
    match event {
        Event::MouseDown {
            button, modifiers, ..
        } => Event::MouseDown {
            x,
            y,
            button,
            modifiers,
        },
        Event::MouseUp {
            button, modifiers, ..
        } => Event::MouseUp {
            x,
            y,
            button,
            modifiers,
        },
        Event::MouseMove { modifiers, .. } => Event::MouseMove { x, y, modifiers },
        Event::MouseDoubleClick {
            button, modifiers, ..
        } => Event::MouseDoubleClick {
            x,
            y,
            button,
            modifiers,
        },
        Event::MouseWheel {
            delta,
            horizontal,
            modifiers,
            ..
        } => Event::MouseWheel {
            delta,
            horizontal,
            x,
            y,
            modifiers,
        },
        other => other,
    }
}
