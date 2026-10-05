//! Reads the app's widget rectangles back from the mounted layout, so a test
//! clicks where the window actually placed a widget.

// Each test binary reads a different subset of the rectangles.
#![allow(dead_code)]

use std::cell::Cell;
use std::rc::Rc;

use xui_core::Ui;
use xui_core::backend::{Result, WidgetId};
use xui_core::geometry::Rect;
use xui_paint::{Msg, PaintApp};

/// The placed widget rectangles, in device pixels.
#[derive(Clone, Copy, Debug)]
pub struct Areas {
    pub toolbar: Rect,
    pub canvas: Rect,
    pub palette: Rect,
    pub status: Rect,
}

/// The widget identities of a built app, shared with a test's step closure.
#[derive(Clone, Default)]
pub struct Widgets(Rc<Cell<Option<[WidgetId; 4]>>>);

impl Widgets {
    /// Remembers the widgets of `app` and passes it through.
    pub fn record(&self, app: Result<PaintApp>) -> Result<PaintApp> {
        if let Ok(app) = &app {
            self.0.set(Some([
                app.toolbar().id(),
                app.canvas().id(),
                app.palette().id(),
                app.status().id(),
            ]));
        }
        app
    }

    /// Where the layout placed each widget.
    pub fn areas(&self, ui: &Ui<Msg>) -> Areas {
        let [toolbar, canvas, palette, status] = self.0.get().expect("the app was built");
        Areas {
            toolbar: ui.bounds(toolbar),
            canvas: ui.bounds(canvas),
            palette: ui.bounds(palette),
            status: ui.bounds(status),
        }
    }
}
