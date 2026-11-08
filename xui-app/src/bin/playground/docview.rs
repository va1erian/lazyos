//! The doc pane's body: a page's [`docs::Block`]s as Markdown in xui's Markdown view
//! (`docs/messenger-playground-plan.md` D6: the schema's own text, nothing invented).
//!
//! The view lays the page out once per width, scrolls by itself and paints only what is on
//! screen; the doc comments in it are Markdown, so their emphasis, code and lists show as written.
//! The window swaps the result into the doc pane with `Panel::set_layout`.

use xui_app::markdown::{style, RhaiCode};
use xui_core::arrange::{build, Build};
use xui_core::Rect;
use xui_markdown::MarkdownView;
use xui_playground::docs::Block;
use xui_playground::markdown;

use crate::msg::Msg;

/// The page of `blocks`, its links reported as [`Msg::OpenLink`].
pub fn view(blocks: &[Block]) -> Build<MarkdownView<Msg>, Msg> {
    let text = markdown::page(blocks);
    build(move |ui| {
        Ok(MarkdownView::new(ui, Rect::default())?
            .style(style())
            .highlighter(RhaiCode)
            .markdown(&text)
            .on_link(|href| Some(Msg::OpenLink(href.to_owned()))))
    })
}
