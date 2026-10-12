#![forbid(unsafe_code)]

//! The two questions Paint asks in modal cards: "Save changes?" (Save /
//! Don't Save / Cancel, LazyWriter's pattern) and "Flatten image?".
//!
//! Both are plain `TaskDialog`s, which hold exactly what these need (text and
//! command buttons), so unlike the Image dialogs they are real dialogs. They
//! are built when first shown, since a `TaskDialog` needs the `Ui`.

use xui_core::app::Ui;
use xui_core::backend::Result;
use xui_core::widget::{TaskDialog, TaskDialogIcon};

use crate::view::Msg;

/// "This picture has unsaved changes": Save (command 0), Don't Save
/// (command 1) or Cancel.
#[derive(Default)]
pub(in crate::view) struct UnsavedPrompt {
    dialog: Option<TaskDialog<Msg>>,
}

impl UnsavedPrompt {
    pub(in crate::view) fn is_open(&self) -> bool {
        self.dialog.as_ref().is_some_and(TaskDialog::is_open)
    }

    /// Closes the card after its answer was handled. A card closes itself when
    /// a button or key dismisses it; this makes a message that arrives by
    /// another route (a script, a test) leave the same state.
    pub(in crate::view) fn close(&self) {
        if let Some(dialog) = &self.dialog {
            dialog.close();
        }
    }

    pub(in crate::view) fn show(&mut self, ui: &Ui<Msg>, name: &str) -> Result<()> {
        // The message names the picture, so the card is built per open.
        let dialog = TaskDialog::new(
            ui,
            "Save changes?",
            &format!("{name} has unsaved changes. Save them before you go on?"),
        )?
        .icon(TaskDialogIcon::Warning)
        .command("Save")?
        .command("Don't Save")?
        .on_action(|action| Some(Msg::Unsaved(action)));
        dialog.open();
        self.dialog = Some(dialog);
        Ok(())
    }
}

/// "Flatten image?": saving a picture with layers in a format that has none.
/// Flatten (command 0) or Cancel.
#[derive(Default)]
pub(in crate::view) struct FlattenPrompt {
    dialog: Option<TaskDialog<Msg>>,
}

impl FlattenPrompt {
    pub(in crate::view) fn is_open(&self) -> bool {
        self.dialog.as_ref().is_some_and(TaskDialog::is_open)
    }

    /// Closes the card after its answer was handled (see [`UnsavedPrompt::close`]).
    pub(in crate::view) fn close(&self) {
        if let Some(dialog) = &self.dialog {
            dialog.close();
        }
    }

    pub(in crate::view) fn show(
        &mut self,
        ui: &Ui<Msg>,
        format: &str,
        layers: usize,
    ) -> Result<()> {
        let dialog = TaskDialog::new(
            ui,
            "Flatten image?",
            &format!(
                "{format} files have no layers. The saved file will be flattened from the \
                 picture's {layers} layers; the picture in Paint keeps them."
            ),
        )?
        .icon(TaskDialogIcon::Warning)
        .command("Flatten")?
        .on_action(|action| Some(Msg::Flatten(action)));
        dialog.open();
        self.dialog = Some(dialog);
        Ok(())
    }
}
