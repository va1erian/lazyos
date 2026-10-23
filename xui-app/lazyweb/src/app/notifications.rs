//! The window's part of web notifications (`crate::notify` is the model): the permission bar
//! above the page, and what the engine's and the shell's news do to it.
//!
//! The bar is the browser's own chrome and not a toast: a toast never takes the keyboard, and an
//! answer to "may this site notify you" must never depend on one. One site is asked at a time;
//! the bar goes when the page that asked is gone.

use std::rc::Rc;
use std::time::Duration;

use xui_core::app::Ui;
use xui_core::geometry::Rect;
use xui_core::widget::{Button, GroupBox, HasText, Label};
use xui_webkit::gesture;

use super::{Browser, Msg};
use crate::chrome::Widgets;
use crate::notify::{BarAnswer, Click, Incoming};

/// How long after a real click or key a request to notify is still the person's (the engine
/// also refuses a request that no gesture started; this is the window's own second look).
const GESTURE_WINDOW: Duration = Duration::from_secs(5);

/// The permission bar's widgets and the site it is asking about.
pub(super) struct Bar {
    frame: Rc<GroupBox<Msg>>,
    text: Rc<Label<Msg>>,
    allow: Rc<Button<Msg>>,
    block: Rc<Button<Msg>>,
    later: Rc<Button<Msg>>,
    /// The host the bar is showing, `None` while it is hidden.
    shown: Option<String>,
}

impl Bar {
    pub(super) fn new(widgets: &Widgets) -> Bar {
        Bar {
            frame: widgets.notify_bar.get(),
            text: widgets.notify_text.get(),
            allow: widgets.notify_allow.get(),
            block: widgets.notify_block.get(),
            later: widgets.notify_later.get(),
            shown: None,
        }
    }

    /// The bar is built up and is hidden until a site asks.
    pub(super) fn start_hidden(&self, ui: &Ui<Msg>) {
        ui.set_visible(self.frame.id(), false);
    }

    /// The site the bar shows, while it is up.
    pub(super) fn shown(&self) -> Option<&str> {
        self.shown.as_deref()
    }

    /// Where the bar and its buttons are, by the names a session script clicks.
    pub(super) fn controls(&self, ui: &Ui<Msg>) -> Vec<(&'static str, Rect)> {
        vec![
            ("notify_bar", ui.bounds(self.frame.id())),
            ("notify_allow", ui.bounds(self.allow.id())),
            ("notify_block", ui.bounds(self.block.id())),
            ("notify_later", ui.bounds(self.later.id())),
        ]
    }
}

impl Browser {
    /// Shows the bar for the site that is asking, or hides it.
    pub(super) fn update_notify_bar(&mut self, ui: &Ui<Msg>) {
        let wanted = self.notifications.asking().map(|site| site.bar_text());
        if wanted == self.bar.shown {
            return;
        }
        if let Some(host) = &wanted {
            self.bar
                .text
                .set_text(&format!("{host} wants to show notifications."));
        }
        ui.set_visible(self.bar.frame.id(), wanted.is_some());
        self.bar.shown = wanted;
        // Where the controls are now, for a session to click.
        ui.relayout();
        self.announce(ui);
    }

    /// The person pressed one of the bar's buttons.
    pub(super) fn notify_answer(&mut self, ui: &Ui<Msg>, answer: BarAnswer) {
        self.notifications
            .answer(self.engine.as_ref(), answer, super::now());
        self.update_notify_bar(ui);
        self.page.focus();
    }

    /// A page called `Notification.requestPermission()`.
    pub(super) fn notify_request(&mut self, ui: &Ui<Msg>, origin: &str, user_gesture: bool) {
        let gesture = user_gesture && gesture::recent(GESTURE_WINDOW);
        self.notifications.request(
            self.engine.as_ref(),
            origin,
            gesture,
            &self.state.committed_url,
        );
        self.update_notify_bar(ui);
    }

    /// A page asked to show a notification.
    pub(super) fn notify_show(&mut self, incoming: &Incoming<'_>) {
        let now = self.started.elapsed().as_millis() as u64;
        self.notifications.show(self.engine.as_ref(), now, incoming);
    }

    /// The shell's worker reported a toast shown, clicked or closed.
    pub(super) fn shell_news(&mut self, ui: &Ui<Msg>) {
        for click in self.notifications.news(self.engine.as_ref()) {
            // A page that has gone: the click brings its site back (the shell has already raised
            // the window).
            if let Click::Open(url) = click {
                self.editing = false;
                self.start_navigation(&url);
            }
        }
        self.update_notify_bar(ui);
    }
}
