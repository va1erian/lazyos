//! The signed-in page: who is signed in, Sign out, and the timeline.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{build as create, button, column, label, row, Handle, LayoutExt, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::widget::Label;
use xui_core::HasText;

use crate::feedview::FeedView;
use crate::Msg;

pub struct HomePage {
    who: Rc<Label<Msg>>,
    status: Rc<Label<Msg>>,
    pub view: Rc<FeedView>,
    _mounted: Mounted<Msg>,
}

impl HomePage {
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<HomePage> {
        let (who, status, view) = (Handle::new(), Handle::new(), Handle::new());
        let mounted = ui.mount_in(
            page,
            column().gap(0).children((
                row().padding(8).gap(8).children((
                    label("").title().bind(&who).fill(1),
                    label("").bind(&status),
                    button("Sign out").on_click(Msg::SignOut),
                )),
                create(move |ui: &Ui<Msg>| FeedView::new(ui))
                    .bind(&view)
                    .fill(1),
            )),
        )?;
        Ok(HomePage {
            who: who.get(),
            status: status.get(),
            view: view.get(),
            _mounted: mounted,
        })
    }

    pub fn set_account(&self, handle: &str) {
        self.who.set_text(&format!("@{handle}"));
        self.say("");
    }

    /// A line beside the title (a failure, a rate limit).
    pub fn say(&self, line: &str) {
        self.status.set_text(line);
    }
}
