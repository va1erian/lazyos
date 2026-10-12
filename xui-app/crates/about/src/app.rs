//! The About window: the view filling it, and the messages the host hears.

use std::rc::Rc;

use xui_core::arrange::{build as create, column, Handle, LayoutExt};
use xui_core::backend::Result;
use xui_core::{App, Ui};

use crate::view::AboutView;

/// The window's design size when a compositor lays the app out.
pub const WINDOW: (i32, i32) = (860, 540);

/// What the panel shows about the build.
#[derive(Clone, Debug)]
pub struct Info {
    pub version: String,
}

/// Something worth a line of serial evidence.
#[derive(Clone, Debug, PartialEq)]
pub enum Report {
    /// Once a second while frames are drawn.
    Frame {
        fps: u32,
        width: usize,
        height: usize,
        divisor: usize,
        millis: f32,
        triangles: usize,
    },
    Shape(&'static str),
    Style(&'static str),
    Page(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    Report(Report),
    Quit,
}

pub struct AboutApp {
    view: Handle<AboutView>,
    report: Box<dyn Fn(&Msg)>,
}

impl AboutApp {
    /// Builds the window in `ui`; `report` hears every message (the LazyOS
    /// binary prints them as serial evidence).
    pub fn build(
        ui: &mut Ui<Msg>,
        info: Info,
        report: impl Fn(&Msg) + 'static,
    ) -> Result<AboutApp> {
        let app = AboutApp {
            view: Handle::new(),
            report: Box::new(report),
        };
        ui.root(
            column().child(
                create(move |ui: &Ui<Msg>| AboutView::new(ui, info.clone()))
                    .bind(&app.view)
                    .fill(1),
            ),
        )?;
        ui.on_close(|| Some(Msg::Quit));
        app.view.get().focus();
        Ok(app)
    }

    /// The view, for a host or a test to drive and inspect.
    pub fn view(&self) -> Rc<AboutView> {
        self.view.get()
    }
}

impl App for AboutApp {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        (self.report)(&msg);
        if msg == Msg::Quit {
            ui.quit();
        }
    }
}
