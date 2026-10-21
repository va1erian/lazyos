//! The reading pane: the message as esMail renders it (sanitised HTML with a
//! header block), drawn by a light view of `webd` (WebKit with no network, no
//! scripts and no storage, docs/webd-plan.md section 3.6).
//!
//! Everything the window asks of the pane goes through [`Reader`], so the
//! engine behind it can change without touching the rest of the app. One view
//! shows every message: a new message is a new page in it (a view per message
//! would leave a parked proxy behind in `webd` each time).

use std::cell::Cell;

use esmail::imap::MailHeader;
use esmail::render::Attachment;
use esmail_glue::reading::{self, Appearance, Palette};
use xui_app::lightview::{LightEvent, LightView};
use xui_core::app::Ui;
use xui_core::backend::{Result, WidgetId};
use xui_core::geometry::Size;
use xui_core::layout::Constraints;
use xui_core::widget::Placeable;
use xui_core::{Color, Rect};

use super::Msg;

/// The view in the pane the window layout places.
pub struct Reader {
    view: LightView<Msg>,
    appearance: Appearance,
    /// Whether the current page is a message whose first paint has not been
    /// reported yet (the session marker).
    awaiting_paint: Cell<bool>,
}

impl Reader {
    pub fn new(ui: &Ui<Msg>) -> Result<Reader> {
        let palette = if ui.theme().is_dark {
            Palette::DARK
        } else {
            Palette::LIGHT
        };
        let appearance = Appearance {
            palette,
            original_colours: false,
        };
        // Links and `mailto:` come back as `Link`: the app decides. Nothing in
        // a message is ever loaded by the view itself.
        let view = LightView::new(
            ui,
            Rect::default(),
            Color::hex(palette.background),
            |event| match event {
                LightEvent::Link(href) => Some(Msg::Link(href)),
                LightEvent::Painted => Some(Msg::Painted),
                LightEvent::Status(status) => Some(Msg::Engine(status)),
                LightEvent::LoadRefused(_) => Some(Msg::Refused),
            },
        )?;
        view.set_html(reading::notice(
            "Select a message to read it.",
            None,
            &palette,
        ));
        Ok(Reader {
            view,
            appearance,
            awaiting_paint: Cell::new(false),
        })
    }

    /// Shows a fetched message. Remote content (images, style sheets, fonts)
    /// is never fetched: a light view has no network at all, so `http(s)`
    /// loads never happen and only inline (`cid:` rewritten to `data:`)
    /// pictures appear. A remote-content toggle would need a full web view
    /// (`os.lazy.webd.view.v1`) with a policy of its own.
    pub fn show_message(&self, header: &MailHeader, body: &str, attachments: &[Attachment]) {
        self.retry_if_failed();
        // As esMail on Windows: the palette's colours unless the message
        // brings its own backgrounds, which then sit on white.
        let themed = self.appearance.themes_body(body);
        let palette = self.appearance.palette;
        self.load(
            self.appearance.page_background(themed),
            reading::document(header, body, attachments, &palette, themed),
        );
        self.awaiting_paint.set(true);
    }

    pub fn show_notice(&self, text: &str) {
        let palette = self.appearance.palette;
        self.load(palette.background, reading::notice(text, None, &palette));
        self.awaiting_paint.set(false);
    }

    /// Try the web engine again if it gave up (a message selected is the cue; there is no key to press).
    pub fn retry_if_failed(&self) {
        if matches!(self.view.status(), xui_webd_client::Status::Failed(_)) {
            self.view.retry();
        }
    }

    /// Shows `notice` in place of a page webd refused.
    pub fn show_refused(&self) {
        self.show_notice("This message is too large to show here.");
    }

    fn load(&self, background: u32, html: String) {
        self.view.set_background(Color::hex(background));
        self.view.set_html(html);
    }

    /// The view reported a painted page: whether it is the message that has
    /// just been shown, for the first time (the session marker).
    pub fn first_paint(&self) -> bool {
        self.awaiting_paint.replace(false)
    }
}

impl Placeable<Msg> for Reader {
    fn id(&self) -> WidgetId {
        self.view.id()
    }

    /// The pane takes whatever its `fill` entry gives it; the layout places
    /// the view's own node.
    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }
}
