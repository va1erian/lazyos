//! The installer's short-lived screens: install progress and success (the
//! wizard's last step), and the remove confirmation. None of them lists
//! anything, so each is a few lines in the middle and its buttons.

use xui_core::arrange::{button, column, label, row, IntoEntry, Layout, LayoutExt};
use xui_core::layout::Align;

use xui_app::installer::{elide, Model, Screen};

use crate::msg::Msg;
use crate::view::MARGIN;
use crate::wizard::{focused, header, nav_button};

/// The lines in the middle of a screen.
fn middle(lines: Vec<String>) -> Layout<Msg> {
    let lines = lines
        .into_iter()
        .map(|line| label(line).into_entry())
        .collect::<Vec<_>>();
    column().gap(8).justify(Align::Center).children(lines)
}

/// The progress screen shown while `pkgd.Install` runs.
pub fn installing(model: &Model) -> Layout<Msg> {
    let name = model
        .inspected
        .as_ref()
        .map(|package| elide(&package.name, 60))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "the package".to_owned());
    column().padding(MARGIN).gap(8).children((
        header(Screen::Installing),
        middle(vec![
            format!("Installing {name}…"),
            "The package service is writing files. This can take a moment.".to_owned(),
        ])
        .fill(1),
    ))
}

/// The success screen after `pkgd.Install` confirmed an app.
pub fn done(model: &Model) -> Layout<Msg> {
    let text = match &model.last_installed {
        Some(app) => format!(
            "{} {} was installed.",
            elide(&app.name, 60),
            elide(&app.version, 20)
        ),
        None => "The package was installed.".to_owned(),
    };
    column().padding(MARGIN).gap(8).children((
        header(Screen::Done),
        middle(vec![text]).fill(1),
        row()
            .justify(Align::End)
            .child(focused(button("Finish").on_click(Msg::Done)).width(100)),
    ))
}

/// The confirmation shown before an app is removed.
pub fn confirm(model: &Model) -> Layout<Msg> {
    let (name, version) = model
        .pending_remove
        .as_ref()
        .map(|app| (elide(&app.name, 60), elide(&app.version, 20)))
        .unwrap_or_else(|| ("the application".to_owned(), String::new()));
    column().padding(MARGIN).gap(8).children((
        middle(vec![
            format!("Remove {name} {version}?"),
            format!(
                "Its files under {} and its help pages are deleted; your documents are kept.",
                fhs::state::APPS_ROOT
            ),
        ])
        .fill(1),
        row().gap(12).justify(Align::End).children((
            nav_button("Remove", Msg::ConfirmRemove),
            nav_button("Cancel", Msg::Cancel),
        )),
    ))
}
