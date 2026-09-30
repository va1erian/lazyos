#![forbid(unsafe_code)]

//! The find/replace session's commands: showing and hiding the bar, the
//! "3 of 17" readout, stepping through matches and replacing.

use xui_code_editor::search;
use xui_core::app::Ui;
use xui_core::widget::HasText;

use crate::app::{Msg, Notepad};

/// Show the find bar and focus its query field.
pub fn show_find(app: &mut Notepad, ui: &mut Ui<Msg>) {
    if app.dialog_open.get() {
        return;
    }
    app.find_open.set(true);
    app.find_bar.set_visible(ui, true);
    app.find_bar.query.focus();
    find_refresh(app, ui);
}

/// Hide the find bar and return focus to the editor.
pub fn close_find(app: &mut Notepad, ui: &mut Ui<Msg>) {
    app.find_open.set(false);
    app.find_bar.set_visible(ui, false);
    app.editor.focus();
}

/// Refresh the "3 of 17" label from the current matches.
pub fn find_refresh(app: &mut Notepad, _ui: &mut Ui<Msg>) {
    let text = app.editor.text();
    match search::matches(&text, &app.search) {
        Err(error) => app.find_bar.status.set_text(&error),
        Ok(found) if found.is_empty() => {
            let label = if app.search.is_empty() {
                String::new()
            } else {
                "No matches".to_string()
            };
            app.find_bar.status.set_text(&label);
        }
        Ok(found) => {
            let label = match search::current_index(&text, &app.search, app.editor.selection()) {
                Some(index) => format!("{} of {}", index + 1, found.len()),
                None => format!("{} matches", found.len()),
            };
            app.find_bar.status.set_text(&label);
        }
    }
}

/// Select the next or previous match, wrapping around.
pub fn find_step(app: &mut Notepad, ui: &mut Ui<Msg>, forward: bool) {
    match app
        .editor
        .find_next(&app.search.query(), app.search.case_sensitive, forward)
    {
        Ok(true) => {
            super::refresh(app, ui);
            find_refresh(app, ui);
        }
        Ok(false) => {
            app.find_bar.status.set_text("No matches");
        }
        Err(error) => app.find_bar.status.set_text(&error),
    }
}

/// Replace the current selection when it is a match, otherwise advance to the
/// next one.
pub fn replace_current(app: &mut Notepad, ui: &mut Ui<Msg>) {
    let text = app.editor.text();
    let selection = app.editor.selection();
    let replacement = app.find_bar.replacement_text();
    match search::replacement_for(&text, &app.search, selection, &replacement) {
        Ok(Some(replaced)) => {
            if let Some((start, end)) = selection {
                app.editor.replace(start, end, &replaced);
            }
            super::refresh(app, ui);
            find_refresh(app, ui);
            find_step(app, ui, true);
        }
        Ok(None) => find_step(app, ui, true),
        Err(error) => app.find_bar.status.set_text(&error),
    }
}

/// Replace every match as a single undo step.
///
/// The new text replaces the whole buffer in one [`Editor::replace`] call, so one
/// Undo restores the original rather than one step per match.
pub fn replace_all(app: &mut Notepad, ui: &mut Ui<Msg>) {
    let text = app.editor.text();
    let replacement = app.find_bar.replacement_text();
    match search::replace_all(&text, &app.search, &replacement) {
        Ok(Some(replaced)) => {
            let chars = text.chars().count();
            app.editor.replace(0, chars, &replaced);
            super::refresh(app, ui);
            find_refresh(app, ui);
        }
        Ok(None) => {}
        Err(error) => app.find_bar.status.set_text(&error),
    }
}
