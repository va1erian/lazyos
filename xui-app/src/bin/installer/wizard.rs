//! The install wizard's chrome and its first step.
//!
//! Every wizard screen shares a [`header`] (the title and a step row with the
//! current step outlined) and, except the progress screen, a [`nav_bar`]
//! (`Back`, `Next`, `Cancel`, right-aligned and equally wide so the forward
//! button never moves between steps). [`choose`] is step 1: a path field and
//! a `Browse…` button that opens the window's file picker.

use xui_core::arrange::{
    button, column, edit, grid, label, row, Build, Entry, IntoEntry, Layout, LayoutExt,
};
use xui_core::layout::{Align, Insets, Track};
use xui_core::widget::{Button, Label};
use xui_core::Dip;

use xui_app::installer::{elide, Model, Screen, WIZARD_STEPS};

use crate::msg::Msg;
use crate::view::MARGIN;

/// The width of a navigation button.
const NAV_W: i32 = 100;

/// The title and the step row.
pub fn header(screen: Screen) -> Layout<Msg> {
    let current = screen.step().unwrap_or(0);
    let title = format!(
        "Install a package  ·  step {} of {}",
        current + 1,
        WIZARD_STEPS.len()
    );
    let steps = WIZARD_STEPS
        .iter()
        .enumerate()
        .map(|(index, name)| {
            label(format!("{}  {name}", index + 1))
                .then(move |step| {
                    // The outline marks the current step.
                    step.set_selected(index == current);
                    step
                })
                .into_entry()
        })
        .collect::<Vec<_>>();
    column().gap(6).children((
        label(title),
        grid(vec![Track::Fill(1); WIZARD_STEPS.len()])
            .gap(8)
            .children(steps),
    ))
}

/// A navigation button `NAV_W` wide.
pub fn nav_button(text: &str, msg: Msg) -> Entry<Msg> {
    button(text).on_click(msg).width(NAV_W)
}

/// Focuses the button once it is created, so `Enter` presses it.
pub fn focused(button: Build<Button<Msg>, Msg>) -> Build<Button<Msg>, Msg> {
    button.then_with(|button, ui| {
        ui.focus(button.id());
        Ok(button)
    })
}

/// The forward button of a wizard step.
pub struct Next {
    /// Its label.
    pub text: &'static str,
    /// What it raises.
    pub msg: Msg,
    /// Whether it can be pressed.
    pub enabled: bool,
    /// Whether it takes the focus, so `Enter` advances.
    pub focus: bool,
}

/// `Back` (only when `back`), the forward button and `Cancel`.
fn nav_bar(back: bool, next: Next) -> Layout<Msg> {
    let enabled = next.enabled;
    let mut forward = button(next.text)
        .then(Button::primary)
        .then(move |button| {
            button.set_enabled(enabled);
            button
        })
        .on_click(next.msg);
    if next.focus {
        forward = focused(forward);
    }
    let back_button = button("< Back")
        .on_click(Msg::Back)
        .then_with(move |button, ui| {
            ui.set_visible(button.id(), back);
            Ok(button)
        });
    row().gap(12).justify(Align::End).children((
        back_button.width(NAV_W),
        forward.width(NAV_W),
        nav_button("Cancel", Msg::Cancel),
    ))
}

/// The status line above the nav bar (an error, or a friendly note).
pub fn banner(model: &Model) -> Build<Label<Msg>, Msg> {
    label(elide(model.banner.as_deref().unwrap_or(""), 160))
}

/// A wizard screen for `screen`: the header, `content` in the leftover
/// height, the banner and the nav bar.
pub fn page(
    screen: Screen,
    model: &Model,
    content: Layout<Msg>,
    back: bool,
    next: Next,
) -> Layout<Msg> {
    column().padding(MARGIN).gap(8).children((
        header(screen),
        content
            .padding(Insets::new(Dip(0.0), Dip(8.0), Dip(0.0), Dip(0.0)))
            .fill(1),
        banner(model),
        nav_bar(back, next),
    ))
}

/// Step 1: choose the package file.
pub fn choose(model: &Model) -> Layout<Msg> {
    let content = column().gap(8).children((
        label("Choose the .lzp package to install."),
        row().gap(8).children((
            edit()
                .text(model.path_input.clone())
                .placeholder("Absolute path to a .lzp package")
                .on_change(Msg::PathChanged)
                // Typing a path is where a keyboard user starts; a mouse user
                // clicks Browse.
                .then(|path| {
                    path.focus();
                    path
                })
                .fill(1),
            button("Browse…").on_click(Msg::Browse).width(104),
        )),
        // pkgd's source rule (docs/packages.md): a user installs from their
        // home folder or /transient, so say so before a refusal does.
        label(format!(
            "Packages install from your home folder or {}. Next shows what it is first.",
            fhs::mount::TRANSIENT
        )),
    ));
    page(
        Screen::Choose,
        model,
        content,
        false,
        Next {
            text: "Next >",
            msg: Msg::Inspect,
            enabled: true,
            focus: false,
        },
    )
}
