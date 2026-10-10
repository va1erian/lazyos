//! The LazyGolf window: the game view filling it, and the messages the
//! host hears.

use xui_core::arrange::{build as create, column, Handle, LayoutExt};
use xui_core::backend::Result;
use xui_core::{App, Ui};

use crate::game::Report;
use crate::view::GolfView;

/// The window's design size when a compositor lays the app out.
pub const WINDOW: (i32, i32) = (1024, 640);

#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    Report(Report),
    Quit,
}

pub struct GolfApp {
    view: Handle<GolfView>,
    report: Box<dyn Fn(&Msg)>,
}

impl GolfApp {
    /// Builds the window in `ui` for course `seed`; `report` hears every
    /// message (the LazyOS binary prints them as serial evidence).
    pub fn build(ui: &mut Ui<Msg>, seed: u64, report: impl Fn(&Msg) + 'static) -> Result<GolfApp> {
        let app = GolfApp {
            view: Handle::new(),
            report: Box::new(report),
        };
        ui.root(
            column().child(
                create(move |ui: &Ui<Msg>| GolfView::new(ui, seed))
                    .bind(&app.view)
                    .fill(1),
            ),
        )?;
        ui.on_close(|| Some(Msg::Quit));
        app.view.get().focus();
        Ok(app)
    }

    /// The game view.
    pub fn view(&self) -> std::rc::Rc<GolfView> {
        self.view.get()
    }
}

impl App for GolfApp {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        (self.report)(&msg);
        if msg == Msg::Quit {
            ui.quit();
        }
    }
}
