//! The signed-in page. Until the FeedView arrives (docs/lazysky-plan.md P3)
//! it shows who is signed in and a plain-text preview of the timeline.

use std::rc::Rc;

use lazysky_core::proto::FeedItem;
use xui_core::app::Ui;
use xui_core::arrange::{button, column, label, row, Handle, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::widget::{Button, Label};
use xui_core::HasText;

use crate::Msg;

/// How many posts the preview lists.
const PREVIEW: usize = 12;

pub struct HomePage {
    who: Rc<Label<Msg>>,
    counts: Rc<Label<Msg>>,
    preview: Rc<Label<Msg>>,
    more: Rc<Button<Msg>>,
    _mounted: Mounted<Msg>,
}

impl HomePage {
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<HomePage> {
        let (who, counts, preview, more) =
            (Handle::new(), Handle::new(), Handle::new(), Handle::new());
        let mounted = ui.mount_in(
            page,
            column().padding(16).gap(8).children((
                label("").title().bind(&who),
                label("").bind(&counts),
                row().gap(8).children((
                    button("Load more").on_click(Msg::LoadMore).bind(&more),
                    button("Sign out").on_click(Msg::SignOut),
                )),
                label("").bind(&preview),
            )),
        )?;
        Ok(HomePage {
            who: who.get(),
            counts: counts.get(),
            preview: preview.get(),
            more: more.get(),
            _mounted: mounted,
        })
    }

    pub fn set_account(&self, handle: &str) {
        self.who.set_text(&format!("Signed in as @{handle}"));
        self.counts.set_text("Loading the timeline...");
        self.preview.set_text("");
    }

    /// Shows the posts loaded so far.
    pub fn set_feed(&self, items: &[FeedItem], skipped: usize, has_more: bool) {
        let note = if skipped > 0 {
            format!(" ({skipped} hidden or unreadable)")
        } else {
            String::new()
        };
        self.counts
            .set_text(&format!("{} posts loaded{note}", items.len()));
        let lines: Vec<String> = items
            .iter()
            .take(PREVIEW)
            .map(|item| {
                let post = &item.post;
                let first = post.text.lines().next().unwrap_or("");
                format!("{}: {first}", post.author.name())
            })
            .collect();
        self.preview.set_text(&lines.join("\n"));
        self.more.set_enabled(has_more);
    }

    pub fn set_loading(&self, loading: bool) {
        self.more.set_enabled(!loading);
    }
}
