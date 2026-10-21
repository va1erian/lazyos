//! The doc pane's body: [`docs::Block`]s in a scroll view
//! (`docs/messenger-playground-plan.md` D6: the schema's own text, nothing
//! invented).
//!
//! The page is one painted widget ([`crate::docbody`]), so scrolling is cheap;
//! the window swaps the result into the doc pane with `Panel::set_layout`.

use xui_core::arrange::{build as create, column, scroll, ScrollBuild};
use xui_playground::docs::Block;

use crate::docbody::DocBody;
use crate::msg::Msg;

/// The blocks as a scrolling page.
pub fn view(blocks: &[Block]) -> ScrollBuild<Msg> {
    let blocks = blocks.to_vec();
    scroll(column().child(create(move |ui| DocBody::new(ui, blocks))))
}
