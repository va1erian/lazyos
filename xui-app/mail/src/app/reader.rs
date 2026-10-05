//! The reading pane: the message as esMail renders it (sanitised HTML with a
//! header block), laid out by an HTML view.
//!
//! Everything the window asks of the pane goes through [`Reader`], so the
//! engine behind it can change without touching the rest of the app.

use std::cell::Cell;

use esmail::imap::MailHeader;
use esmail::render::Attachment;
use esmail_glue::reading::{self, Appearance, Palette};
use xui_core::app::Ui;
use xui_core::backend::{NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::Size;
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::{Color, Rect};
use xui_litehtml::{HtmlView, HtmlViewEvent};

use super::Msg;

/// The view in a frame the window layout places: the view exposes no node of
/// its own to place (as LazyWeb's page).
pub struct Reader {
    frame: Control<Msg>,
    view: HtmlView<Msg>,
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
        let frame = Control::new(ui, &NodeSpec::new(NodeKind::Container, Rect::default()))?;
        let html = reading::notice("Select a message to read it.", None, &palette);
        // litehtml lays the page out at the view's first size, so start near
        // the final one rather than at nothing.
        let start = Rect::from_size(ui.client_rect().size());
        let view = HtmlView::new(
            &ui.with_parent(frame.id()),
            start,
            html,
            || Msg::Frame,
            |event| match event {
                HtmlViewEvent::LinkClicked(href) => Some(Msg::Link(href)),
                HtmlViewEvent::CopyRequested(text) => Some(Msg::Copy(text)),
            },
        )?;
        view.set_background(Color::hex(palette.background));
        Ok(Reader {
            frame,
            view,
            appearance,
            awaiting_paint: Cell::new(false),
        })
    }

    /// Shows a fetched message. Remote images are never fetched: the view has
    /// no image fetcher, so only inline (`cid:`/`data:`) pictures appear.
    pub fn show_message(&self, header: &MailHeader, body: &str, attachments: &[Attachment]) {
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

    fn load(&self, background: u32, html: String) {
        self.view.set_background(Color::hex(background));
        self.view.load(html);
    }

    /// A layout pass finished: repaint it.
    pub fn frame(&self) {
        self.view.invalidate();
    }

    /// Whether the message on show has just been painted for the first time.
    /// The view takes a finished layout when it paints, so this is polled (a
    /// timer) rather than answered when the layout is reported.
    pub fn first_paint(&self) -> bool {
        let first = self.awaiting_paint.get() && self.view.is_ready();
        if first {
            self.awaiting_paint.set(false);
        }
        first
    }
}

impl Placeable<Msg> for Reader {
    fn id(&self) -> WidgetId {
        self.frame.id()
    }

    /// The pane takes whatever its `fill` entry gives it.
    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }

    /// The view fills the frame.
    fn placed(&self, _ui: &Ui<Msg>, rect: Rect) {
        self.view.set_bounds(Rect::from_size(rect.size()));
        self.view.invalidate();
    }
}
