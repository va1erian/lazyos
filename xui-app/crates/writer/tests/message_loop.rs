//! LazyWriter's message loop driven headlessly: the modified flag, the
//! Save / Discard / Cancel prompt, saving, opening a bad file and the
//! dialog gate on the window's shortcuts. Each test runs under a watchdog.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use xui_canvas::snapshot::Stage;
use xui_core::Theme;
use xui_core::backend::Event;
use xui_core::message::{Key, Modifiers};
use xui_core::widget::TaskDialogAction;
use xui_rich_text::edit::Command;
use xui_writer::Msg;

use common::{Rig, TempDir, pump, render, watchdog};

/// Runs `step` in a live window over a fresh folder; returns what it recorded.
fn drive(
    step: impl FnOnce(&Stage<'_, Msg>, &Rig, &TempDir) -> Vec<String> + Send + 'static,
) -> Vec<String> {
    watchdog(|| {
        let dir = Rc::new(TempDir::new("loop"));
        let out: Rc<RefCell<Vec<String>>> = Rc::default();
        let (keep, folder) = (Rc::clone(&out), Rc::clone(&dir));
        render(Theme::light(), dir.0.clone(), move |stage, rig| {
            *keep.borrow_mut() = step(stage, rig, &folder);
        });
        out.take()
    })
}

fn type_text(stage: &Stage<'_, Msg>, rig: &Rig, text: &str) {
    rig.editor.exec(Command::InsertText(text.to_owned()));
    pump(stage);
}

fn key(stage: &Stage<'_, Msg>, key: Key, modifiers: Modifiers) {
    stage.inject(Event::KeyDown {
        key,
        modifiers,
        repeat: 1,
        system: false,
    });
}

const CTRL: Modifiers = Modifiers {
    ctrl: true,
    ..Modifiers::NONE
};

#[test]
fn typing_marks_the_document_modified_and_counts_words() {
    let seen = drive(|stage, rig, _| {
        let before = rig.status(1);
        type_text(stage, rig, "Two words");
        vec![before, rig.status(1), rig.status(2)]
    });
    assert_eq!(seen, ["Saved", "Modified", "2 words"]);
}

#[test]
fn new_on_a_modified_document_asks_and_discard_clears_it() {
    let seen = drive(|stage, rig, _| {
        type_text(stage, rig, "draft");
        stage.emit(Msg::New);
        let asked = rig.dialog_open.get();
        stage.emit(Msg::Unsaved(TaskDialogAction::Command(1)));
        vec![
            asked.to_string(),
            rig.dialog_open.get().to_string(),
            rig.text(),
            rig.status(1),
        ]
    });
    assert_eq!(seen, ["true", "false", "", "Saved"]);
}

#[test]
fn cancel_keeps_the_modified_document() {
    let seen = drive(|stage, rig, _| {
        type_text(stage, rig, "keep me");
        stage.emit(Msg::Quit);
        stage.emit(Msg::Unsaved(TaskDialogAction::Cancel));
        vec![rig.text(), rig.status(1)]
    });
    assert_eq!(seen, ["keep me", "Modified"]);
}

#[test]
fn save_writes_lzw_and_save_in_the_prompt_saves_before_new() {
    let seen = drive(|stage, rig, dir| {
        type_text(stage, rig, "first");
        // A name typed without an extension gets `.lzw`.
        stage.emit(Msg::SaveChosen(dir.file("letter")));
        let saved = dir.file("letter.lzw");
        let after_save = (rig.status(0), rig.status(1));
        type_text(stage, rig, " second");
        stage.emit(Msg::New);
        stage.emit(Msg::Unsaved(TaskDialogAction::Command(0)));
        let reopened = xui_writer::files::open_document(&saved).expect("saved document");
        vec![
            after_save.0,
            after_save.1,
            reopened.to_plain_text(),
            rig.text(),
        ]
    });
    assert_eq!(seen, ["letter.lzw", "Saved", "first second", ""]);
}

#[test]
fn a_bad_file_shows_a_message_and_keeps_the_document() {
    let seen = drive(|stage, rig, dir| {
        type_text(stage, rig, "mine");
        let bad = dir.file("bad.lzw");
        std::fs::write(&bad, "{ not json").unwrap();
        stage.emit(Msg::OpenChosen(bad));
        let shown = rig.dialog_open.get();
        stage.emit(Msg::MessageClosed);
        vec![
            shown.to_string(),
            rig.dialog_open.get().to_string(),
            rig.text(),
        ]
    });
    assert_eq!(seen, ["true", "false", "mine"]);
}

#[test]
fn a_saved_document_opens_again() {
    let seen = drive(|stage, rig, dir| {
        type_text(stage, rig, "round trip");
        stage.emit(Msg::SaveChosen(dir.file("trip.lzw")));
        stage.emit(Msg::New);
        let cleared = rig.text();
        stage.emit(Msg::OpenChosen(dir.file("trip.lzw")));
        vec![cleared, rig.text(), rig.status(0), rig.status(1)]
    });
    assert_eq!(seen, ["", "round trip", "trip.lzw", "Saved"]);
}

#[test]
fn export_writes_markdown_and_leaves_the_document_saved_state_alone() {
    let seen = drive(|stage, rig, dir| {
        type_text(stage, rig, "exported");
        stage.emit(Msg::SaveChosen(dir.file("x.lzw")));
        stage.emit(Msg::ExportChosen(dir.file("x")));
        let md = std::fs::read_to_string(dir.file("x.md")).unwrap_or_default();
        vec![md.trim().to_owned(), rig.status(1)]
    });
    assert_eq!(seen, ["exported", "Saved"]);
}

#[test]
fn ctrl_s_opens_save_as_and_shortcuts_wait_while_it_is_open() {
    let seen = drive(|stage, rig, _| {
        type_text(stage, rig, "untitled text");
        key(stage, Key::S, CTRL);
        let open = rig.dialog_open.get();
        // Ctrl+N would ask about unsaved changes; with Save As open it must not.
        key(stage, Key::N, CTRL);
        vec![open.to_string(), rig.text()]
    });
    assert_eq!(seen, ["true", "untitled text"]);
}

#[test]
fn a_page_break_adds_a_page_to_the_status_bar() {
    let seen = drive(|stage, rig, _| {
        type_text(stage, rig, "First page");
        let before = rig.status(3);
        stage.emit(Msg::PageBreak);
        pump(stage);
        type_text(stage, rig, "Second page");
        vec![before, rig.status(3)]
    });
    assert_eq!(seen, ["Page 1 of 1", "Page 2 of 2"]);
}

#[test]
fn page_setup_choices_change_the_page_as_one_undo_step() {
    let seen = drive(|stage, rig, _| {
        let landscape = || rig.editor.with_document(|d| d.page().is_landscape());
        let before = landscape();
        // 4 is Landscape in the Page setup menu.
        stage.emit(Msg::PageChoice(4));
        pump(stage);
        let after = landscape();
        stage.emit(Msg::Undo);
        pump(stage);
        vec![before, after, landscape()]
            .into_iter()
            .map(|b| b.to_string())
            .collect()
    });
    assert_eq!(seen, ["false", "true", "false"]);
}

#[test]
fn the_view_toggle_switches_to_draft_and_back() {
    let seen = drive(|stage, rig, _| {
        let mode = || format!("{:?}", rig.editor.current_view_mode());
        let first = mode();
        stage.emit(Msg::PageView(false));
        pump(stage);
        let draft = mode();
        stage.emit(Msg::PageView(true));
        pump(stage);
        vec![first, draft, mode()]
    });
    assert_eq!(seen, ["Page", "Draft", "Page"]);
}
