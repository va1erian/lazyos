//! LazySky's portable window (docs/lazysky-plan.md §2): xui code that
//! depends on `lazysky-core` and `xui-core` only. The LazyOS binary and the
//! desktop example (`examples/desktop.rs`) launch it and supply a
//! [`lazysky_core::Platform`]; nothing here may call an OS API.
//!
//! P0 holds the spike: [`spike::SpikeView`] shapes text on a worker thread
//! and draws it with `Canvas::draw_layout`, the risk the feed (§6) rests on.

pub mod spike;

use std::sync::Arc;

use lazysky_core::Platform;
use xui_core::arrange::{build as create, column, Handle, LayoutExt};
use xui_core::backend::Result;
use xui_core::{App, Ui};

use spike::SpikeView;

/// The window's design size when a compositor lays the app out.
pub const WINDOW: (i32, i32) = (960, 640);

/// What the window tells its host (the binary prints it as serial evidence).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// The spike's paragraph was shaped on a worker thread and arrived.
    ShapedOffThread { lines_height: i32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    Report(Report),
    /// The view's timer: look for what the worker threads finished.
    Poll,
    Quit,
}

pub struct LazySkyApp {
    #[allow(dead_code)]
    platform: Arc<dyn Platform>,
    view: Handle<SpikeView>,
    report: Box<dyn Fn(&Msg)>,
}

impl LazySkyApp {
    /// Builds the window in `ui`; `report` hears every message.
    pub fn build(
        ui: &mut Ui<Msg>,
        platform: Arc<dyn Platform>,
        report: impl Fn(&Msg) + 'static,
    ) -> Result<LazySkyApp> {
        let app = LazySkyApp {
            platform,
            view: Handle::new(),
            report: Box::new(report),
        };
        ui.root(
            column().child(
                create(move |ui: &Ui<Msg>| SpikeView::new(ui))
                    .bind(&app.view)
                    .fill(1),
            ),
        )?;
        ui.on_close(|| Some(Msg::Quit));
        Ok(app)
    }

    /// The spike view.
    pub fn view(&self) -> std::rc::Rc<SpikeView> {
        self.view.get()
    }
}

impl App for LazySkyApp {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Poll => {
                if let Some(report) = self.view.get().poll() {
                    (self.report)(&Msg::Report(report));
                }
            }
            Msg::Quit => {
                (self.report)(&Msg::Quit);
                ui.quit();
            }
            Msg::Report(_) => (self.report)(&msg),
        }
    }
}
