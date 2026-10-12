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

impl Areas {
    /// The window position of the top-left of picture pixel `at` of a
    /// `doc`-sized picture at 100%, through the real viewport mapping: the
    /// picture is centred on each axis where it fits the canvas.
    pub fn picture(&self, doc: (u32, u32), at: (i32, i32)) -> (i32, i32) {
        let mut viewport = xui_paint::viewport::Viewport::new();
        viewport.clamp(doc, (self.canvas.width(), self.canvas.height()));
        let (x, y) = viewport.to_view(at.0, at.1);
        (self.canvas.left + x, self.canvas.top + y)
    }

    /// Like [`Areas::picture`] for the default 320 x 240 picture.
    pub fn pixel(&self, x: i32, y: i32) -> (i32, i32) {
        self.picture((320, 240), (x, y))
    }
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
