//! The window's part of "this page is not responding" (`crate::hang` is the model): a bar above
//! the page with Wait, Reload and Stop. It is the browser's own chrome and is drawn by the window's
//! thread, which the stuck page cannot hold up.
//!
//! Serial evidence: `WEBPLUS:HANG:SHOW`, `WEBPLUS:HANG:HIDE`, `WEBPLUS:HANG:ANSWER:wait|reload|stop`.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::geometry::Rect;
use xui_core::widget::{Button, GroupBox, Label};

use super::{Browser, Msg};
use crate::chrome::Widgets;
use crate::hang::{Answer, Watch};

/// The bar's widgets and the question's state.
pub(super) struct Bar {
    frame: Rc<GroupBox<Msg>>,
    wait: Rc<Button<Msg>>,
    reload: Rc<Button<Msg>>,
    stop: Rc<Button<Msg>>,
    _text: Rc<Label<Msg>>,
    watch: Watch,
}

impl Bar {
    pub(super) fn new(widgets: &Widgets) -> Bar {
        Bar {
            frame: widgets.hang_bar.get(),
            wait: widgets.hang_wait.get(),
            reload: widgets.hang_reload.get(),
            stop: widgets.hang_stop.get(),
            _text: widgets.hang_text.get(),
            watch: Watch::default(),
        }
    }

    pub(super) fn start_hidden(&self, ui: &Ui<Msg>) {
        ui.set_visible(self.frame.id(), false);
    }

    pub(super) fn visible(&self) -> bool {
        self.watch.visible()
    }

    pub(super) fn stuck(&self) -> bool {
        self.watch.stuck()
    }

    /// Where the bar and its buttons are, by the names a session script clicks.
    pub(super) fn controls(&self, ui: &Ui<Msg>) -> Vec<(&'static str, Rect)> {
        vec![
            ("hang_bar", ui.bounds(self.frame.id())),
            ("hang_wait", ui.bounds(self.wait.id())),
            ("hang_reload", ui.bounds(self.reload.id())),
            ("hang_stop", ui.bounds(self.stop.id())),
        ]
    }
}

impl Browser {
    /// The engine says the page's process answers, or does not.
    pub(super) fn page_responsive(&mut self, ui: &Ui<Msg>, responsive: bool) {
        if self.hang.watch.stuck() == responsive {
            self.note_engine(if responsive {
                "page answering again"
            } else {
                "page not responding"
            });
        }
        if self.hang.watch.responsive(responsive) {
            self.show_hang_bar(ui);
        }
    }

    fn show_hang_bar(&mut self, ui: &Ui<Msg>) {
        let visible = self.hang.visible();
        ui.set_visible(self.hang.frame.id(), visible);
        crate::marker(if visible { "HANG:SHOW" } else { "HANG:HIDE" });
        ui.relayout();
        self.announce(ui);
    }

    /// The person pressed one of the bar's buttons.
    pub(super) fn hang_answer(&mut self, ui: &Ui<Msg>, answer: Answer) {
        crate::marker(&format!("HANG:ANSWER:{}", answer.name()));
        self.note_engine(&format!("answered: {}", answer.name()));
        if let Some(reload) = self.hang.watch.answer(answer) {
            self.engine.recover(reload);
        }
        self.show_hang_bar(ui);
        self.page.focus();
    }
}
