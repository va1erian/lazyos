//! The installer's short-lived screens: install progress, install success, and
//! the remove confirmation. None of them lists anything, so each is a panel and
//! a few labels and buttons.

use xui_core::app::Ui;
use xui_core::widget::{Button, Label, Panel};

use xui_app::installer::{elide, Model};

use crate::msg::Msg;
use crate::view::{fail, rect, MARGIN};

/// The progress screen shown while `pkgd.Install` runs.
pub struct InstallingScreen {
    _panel: Panel<Msg>,
    _message: Label<Msg>,
    _hint: Label<Msg>,
}

impl InstallingScreen {
    /// Builds the screen at `width` x `height` from `model`.
    pub fn build(
        ui: &Ui<Msg>,
        width: i32,
        height: i32,
        model: &Model,
    ) -> Result<InstallingScreen, String> {
        let panel = Panel::new(ui, rect(0, 0, width, height)).map_err(fail)?;
        let page = panel.ui();
        let name = model
            .inspected
            .as_ref()
            .map(|package| elide(&package.name, 60))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "the package".to_owned());
        let message = Label::new(
            page,
            rect(MARGIN, height / 2 - 30, width - 2 * MARGIN, 24),
            &format!("Installing {name}…"),
        )
        .map_err(fail)?;
        let hint = Label::new(
            page,
            rect(MARGIN, height / 2, width - 2 * MARGIN, 16),
            "The package service is writing files. This can take a moment.",
        )
        .map_err(fail)?;
        Ok(InstallingScreen {
            _panel: panel,
            _message: message,
            _hint: hint,
        })
    }
}

/// The success screen after `pkgd.Install` confirmed an app.
pub struct DoneScreen {
    _panel: Panel<Msg>,
    _message: Label<Msg>,
    _done: Button<Msg>,
}

impl DoneScreen {
    /// Builds the screen at `width` x `height` from `model`.
    pub fn build(
        ui: &Ui<Msg>,
        width: i32,
        height: i32,
        model: &Model,
    ) -> Result<DoneScreen, String> {
        let panel = Panel::new(ui, rect(0, 0, width, height)).map_err(fail)?;
        let page = panel.ui();
        let text = match &model.last_installed {
            Some(app) => format!(
                "{} {} was installed.",
                elide(&app.name, 60),
                elide(&app.version, 20)
            ),
            None => "The package was installed.".to_owned(),
        };
        let message = Label::new(
            page,
            rect(MARGIN, height / 2 - 30, width - 2 * MARGIN, 24),
            &text,
        )
        .map_err(fail)?;
        let done = Button::new(
            page,
            rect(width - MARGIN - 116, height - 48, 100, 30),
            "Done",
        )
        .map_err(fail)?
        .on_click(|| Some(Msg::Done));
        Ok(DoneScreen {
            _panel: panel,
            _message: message,
            _done: done,
        })
    }
}

/// The confirmation shown before an app is removed.
pub struct ConfirmScreen {
    _panel: Panel<Msg>,
    _message: Label<Msg>,
    _detail: Label<Msg>,
    _remove: Button<Msg>,
    _cancel: Button<Msg>,
}

impl ConfirmScreen {
    /// Builds the screen at `width` x `height` from `model`.
    pub fn build(
        ui: &Ui<Msg>,
        width: i32,
        height: i32,
        model: &Model,
    ) -> Result<ConfirmScreen, String> {
        let panel = Panel::new(ui, rect(0, 0, width, height)).map_err(fail)?;
        let page = panel.ui();
        let (name, version) = model
            .pending_remove
            .as_ref()
            .map(|app| (elide(&app.name, 60), elide(&app.version, 20)))
            .unwrap_or_else(|| ("the application".to_owned(), String::new()));
        let message = Label::new(
            page,
            rect(MARGIN, height / 2 - 44, width - 2 * MARGIN, 24),
            &format!("Remove {name} {version}?"),
        )
        .map_err(fail)?;
        let detail = Label::new(
            page,
            rect(MARGIN, height / 2 - 14, width - 2 * MARGIN, 16),
            "Its files under /data/apps are deleted; your documents are kept.",
        )
        .map_err(fail)?;
        let remove = Button::new(
            page,
            rect(width - MARGIN - 228, height - 48, 100, 30),
            "Remove",
        )
        .map_err(fail)?
        .on_click(|| Some(Msg::ConfirmRemove));
        let cancel = Button::new(
            page,
            rect(width - MARGIN - 116, height - 48, 100, 30),
            "Cancel",
        )
        .map_err(fail)?
        .on_click(|| Some(Msg::Cancel));
        Ok(ConfirmScreen {
            _panel: panel,
            _message: message,
            _detail: detail,
            _remove: remove,
            _cancel: cancel,
        })
    }
}
