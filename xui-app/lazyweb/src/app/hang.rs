//! The window's part of "this page is not responding" (`crate::hang` is the model): a bar above
//! the page with Wait, Reload and Stop. It is the browser's own chrome and is drawn by the window's
//! thread, which the stuck page cannot hold up.
//!
//! The same bar is the crashed-tab page: a tab whose view was lost (`ViewLost`) says "This tab
//! stopped (why)" with Reload alone, which makes a new view; nothing is loaded until it is pressed.
//!
//! Serial evidence: `LAZYWEB:HANG:SHOW`, `LAZYWEB:HANG:HIDE`, `LAZYWEB:HANG:ANSWER:wait|reload|stop`,
//! `LAZYWEB:TAB:CRASHED:SHOW|HIDE`.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::geometry::Rect;
use xui_core::widget::{Button, GroupBox, HasText, Label};

use super::{Browser, Msg};
use crate::chrome::Widgets;
use crate::hang::{Answer, Watch};

/// The bar's widgets and the question's state.
pub(super) struct Bar {
    frame: Rc<GroupBox<Msg>>,
    wait: Rc<Button<Msg>>,
    reload: Rc<Button<Msg>>,
    stop: Rc<Button<Msg>>,
    text: Rc<Label<Msg>>,
    watch: Watch,
    /// Why the front tab's view is gone, while it is (the bar is the crashed-tab page then).
    crashed: Option<&'static str>,
    /// Whether the bar is up, so a tab switch that changes nothing prints nothing.
    shown: bool,
}

impl Bar {
    pub(super) fn new(widgets: &Widgets) -> Bar {
        Bar {
            frame: widgets.hang_bar.get(),
            wait: widgets.hang_wait.get(),
            reload: widgets.hang_reload.get(),
            stop: widgets.hang_stop.get(),
            text: widgets.hang_text.get(),
            watch: Watch::default(),
            crashed: None,
            shown: false,
        }
    }

    pub(super) fn start_hidden(&self, ui: &Ui<Msg>) {
        ui.set_visible(self.frame.id(), false);
    }

    pub(super) fn visible(&self) -> bool {
        self.watch.visible() || self.crashed.is_some()
    }

    /// The front tab shows the crashed-tab page.
    pub(super) fn is_crashed(&self) -> bool {
        self.crashed.is_some()
    }

    /// A page that was stuck in another tab, or none: the watch starts again from there.
    pub(super) fn reset(&mut self, stuck: bool) {
        self.watch = Watch::default();
        if stuck {
            self.watch.responsive(false);
        }
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

    pub(super) fn show_hang_bar(&mut self, ui: &Ui<Msg>) {
        let visible = self.hang.visible();
        let crashed = self.hang.crashed;
        let text = match crashed {
            Some(why) => format!("This tab stopped ({why})."),
            None => String::from("This page is not responding."),
        };
        self.hang.text.set_text(&text);
        // Wait and Stop mean nothing for a view that is gone: only Reload is left.
        ui.set_visible(self.hang.wait.id(), crashed.is_none());
        ui.set_visible(self.hang.stop.id(), crashed.is_none());
        ui.set_visible(self.hang.frame.id(), visible);
        if visible != self.hang.shown {
            let word = if crashed.is_some() {
                "TAB:CRASHED:"
            } else {
                "HANG:"
            };
            crate::marker(&format!("{word}{}", if visible { "SHOW" } else { "HIDE" }));
            self.hang.shown = visible;
        }
        ui.relayout();
        self.announce(ui);
    }

    /// The front tab's view is lost (`why` is one of a few fixed words) or has a new one (`None`).
    pub(super) fn set_crashed_page(&mut self, ui: &Ui<Msg>, why: Option<&'static str>) {
        if self.hang.crashed == why {
            return;
        }
        self.hang.crashed = why;
        self.show_hang_bar(ui);
    }

    /// The person pressed one of the bar's buttons.
    pub(super) fn hang_answer(&mut self, ui: &Ui<Msg>, answer: Answer) {
        crate::marker(&format!("HANG:ANSWER:{}", answer.name()));
        self.note_engine(&format!("answered: {}", answer.name()));
        if self.hang.is_crashed() {
            // A new view loads the tab's page again; the bar goes when the state says it did.
            self.engine.reload();
        } else if let Some(reload) = self.hang.watch.answer(answer) {
            self.engine.recover(reload);
        }
        self.show_hang_bar(ui);
        self.page.focus();
    }
}
