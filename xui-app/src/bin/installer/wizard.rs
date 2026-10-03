//! The install wizard's chrome and its first step.
//!
//! Every wizard screen shares a [`Header`] (the title and a step row with the
//! current step outlined) and, except the progress screen, a [`NavBar`]
//! (`Back`, `Next`, `Cancel`, right-aligned so the forward button never moves
//! between steps). [`ChooseScreen`] is step 1: a path field and a `Browse…`
//! button that opens the window's file picker.

use xui_core::app::Ui;
use xui_core::widget::{Button, Edit, Label, Panel};

use xui_app::installer::{elide, Model, Screen, WIZARD_STEPS};

use crate::msg::Msg;
use crate::view::{fail, rect, MARGIN};

/// Where a wizard screen's own content starts, below the [`Header`].
pub const CONTENT_TOP: i32 = 72;
/// The height a wizard screen leaves at the bottom for the banner and the
/// [`NavBar`].
pub const FOOTER_H: i32 = 80;

/// The title and the step row.
pub struct Header {
    _title: Label<Msg>,
    _steps: Vec<Label<Msg>>,
}

impl Header {
    /// Builds the header on `page` for `screen`'s step.
    pub fn build(page: &Ui<Msg>, width: i32, screen: Screen) -> Result<Header, String> {
        let current = screen.step().unwrap_or(0);
        let title_text = format!(
            "Install a package  ·  step {} of {}",
            current + 1,
            WIZARD_STEPS.len()
        );
        let title = Label::new(page, rect(MARGIN, 10, width - 2 * MARGIN, 22), &title_text)
            .map_err(fail)?;
        let count = WIZARD_STEPS.len() as i32;
        let slot = (width - 2 * MARGIN) / count;
        let mut steps = Vec::new();
        for (index, name) in WIZARD_STEPS.iter().enumerate() {
            let left = MARGIN + slot * index as i32;
            let text = format!("{}  {name}", index + 1);
            let label = Label::new(page, rect(left + 2, 38, slot - 8, 22), &text).map_err(fail)?;
            // The outline marks the current step.
            label.set_selected(index == current);
            steps.push(label);
        }
        Ok(Header {
            _title: title,
            _steps: steps,
        })
    }
}

/// `Back`, the forward button and `Cancel`.
pub struct NavBar {
    _back: Button<Msg>,
    next: Button<Msg>,
    _cancel: Button<Msg>,
}

impl NavBar {
    /// Builds the bar on `page`. `next` is the forward button's label and
    /// message; `back` hides `Back` on the first step.
    pub fn build(
        page: &Ui<Msg>,
        width: i32,
        height: i32,
        back: bool,
        next: (&str, Msg),
    ) -> Result<NavBar, String> {
        let top = height - 48;
        let back_button = Button::new(page, rect(width - MARGIN - 340, top, 100, 30), "< Back")
            .map_err(fail)?
            .on_click(|| Some(Msg::Back));
        page.set_visible(back_button.id(), back);
        let (label, message) = next;
        let next_button = Button::new(page, rect(width - MARGIN - 228, top, 100, 30), label)
            .map_err(fail)?
            .primary()
            .on_click(move || Some(message.clone()));
        let cancel = Button::new(page, rect(width - MARGIN - 116, top, 100, 30), "Cancel")
            .map_err(fail)?
            .on_click(|| Some(Msg::Cancel));
        Ok(NavBar {
            _back: back_button,
            next: next_button,
            _cancel: cancel,
        })
    }

    /// Enables or disables the forward button.
    pub fn set_next_enabled(&self, enabled: bool) {
        self.next.set_enabled(enabled);
    }

    /// Focuses the forward button, so `Enter` advances.
    pub fn focus_next(&self, page: &Ui<Msg>) {
        page.focus(self.next.id());
    }
}

/// The status line above the nav bar (an error, or a friendly note).
pub fn banner(
    page: &Ui<Msg>,
    width: i32,
    height: i32,
    model: &Model,
) -> Result<Label<Msg>, String> {
    let text = model.banner.as_deref().unwrap_or("");
    Label::new(
        page,
        rect(MARGIN, height - FOOTER_H + 6, width - 2 * MARGIN, 18),
        &elide(text, 160),
    )
    .map_err(fail)
}

/// Step 1: choose the package file.
pub struct ChooseScreen {
    _panel: Panel<Msg>,
    _header: Header,
    _prompt: Label<Msg>,
    _path: Edit<Msg>,
    _browse: Button<Msg>,
    _hint: Label<Msg>,
    _banner: Label<Msg>,
    _nav: NavBar,
}

impl ChooseScreen {
    /// Builds the screen at `width` x `height` from `model`.
    pub fn build(
        ui: &Ui<Msg>,
        width: i32,
        height: i32,
        model: &Model,
    ) -> Result<ChooseScreen, String> {
        let panel = Panel::new(ui, rect(0, 0, width, height)).map_err(fail)?;
        let page = panel.ui();
        let header = Header::build(page, width, Screen::Choose)?;

        let prompt = Label::new(
            page,
            rect(MARGIN, CONTENT_TOP + 8, width - 2 * MARGIN, 18),
            "Choose the .lzp package to install.",
        )
        .map_err(fail)?;
        let edit_w = (width - 2 * MARGIN - 112).max(80);
        let path = Edit::new(
            page,
            rect(MARGIN, CONTENT_TOP + 34, edit_w, 26),
            &model.path_input,
        )
        .map_err(fail)?
        .cue("Absolute path to a .lzp package")
        .on_change(|text| Some(Msg::PathChanged(text.to_owned())));
        let browse = Button::new(
            page,
            rect(MARGIN + edit_w + 8, CONTENT_TOP + 34, 104, 26),
            "Browse…",
        )
        .map_err(fail)?
        .on_click(|| Some(Msg::Browse));
        // pkgd's source rule (docs/packages.md): a user installs from their
        // home folder or /transient, so say so before a refusal does.
        let hint = Label::new(
            page,
            rect(MARGIN, CONTENT_TOP + 72, width - 2 * MARGIN, 16),
            &format!(
                "Packages install from your home folder or {}. Next shows what it is first.",
                fhs::mount::TRANSIENT
            ),
        )
        .map_err(fail)?;

        let banner = banner(page, width, height, model)?;
        let nav = NavBar::build(page, width, height, false, ("Next >", Msg::Inspect))?;
        // Typing a path is where a keyboard user starts; a mouse user clicks
        // Browse.
        path.focus();
        Ok(ChooseScreen {
            _panel: panel,
            _header: header,
            _prompt: prompt,
            _path: path,
            _browse: browse,
            _hint: hint,
            _banner: banner,
            _nav: nav,
        })
    }
}
