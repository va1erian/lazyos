#![forbid(unsafe_code)]

//! LazyWriter's formatting commands and the title/status refresh, ported from
//! the wordpad example's `commands.rs`. The file commands and dialogs live in
//! [`files`].

pub mod files;

use xui_core::Dip;
use xui_core::app::Ui;
use xui_core::widget::DialogAction;
use xui_rich_text::edit::Command;
use xui_rich_text::model::{BlockKind, CharStylePatch, Selection, Side, StyleSummary, Wrap};

use crate::app::{Mark, Msg, Writer};
use crate::names;
use crate::ui::{BLOCKS, FAMILIES, SIZES, WRAPS};

/// The window title: `LazyWriter: <name>`, with `*` before the name when the
/// document is modified.
pub fn title(name: &str, dirty: bool) -> String {
    format!("LazyWriter: {}{name}", if dirty { "*" } else { "" })
}

/// The status bar's word count.
pub fn words_label(words: usize) -> String {
    match words {
        1 => "1 word".to_owned(),
        n => format!("{n} words"),
    }
}

/// Refreshes the title bar and the status bar's name and state.
pub fn refresh_title(app: &Writer, ui: &Ui<Msg>) {
    let name = names::display_name(app.path.as_deref());
    ui.set_window_title(&title(&name, app.dirty));
    app.status.set_text(0, &name);
    app.status
        .set_text(1, if app.dirty { "Modified" } else { "Saved" });
}

/// The document changed.
pub fn edited(app: &mut Writer, ui: &mut Ui<Msg>, words: usize) {
    app.dirty = true;
    app.status.set_text(2, &words_label(words));
    refresh_title(app, ui);
}

/// The selection or its formatting changed: update the toolbar.
pub fn selection(app: &mut Writer, summary: StyleSummary) {
    app.tools.sync(&summary, &app.host);
    app.summary = Some(summary);
    // `StyleSummary` has no wrap: read the selected image's from the document.
    let image = match app.editor.selection() {
        Selection::Object(id) => app
            .editor
            .with_document(|d| d.objects().get(id).map(|o| o.wrap)),
        Selection::Text { .. } => None,
    };
    app.tools.sync_wrap(image);
}

/// Runs an editor command from the toolbar and returns focus to the text.
///
/// The last summary is re-applied afterwards: a click on a checked alignment
/// toggle unchecks the button but changes nothing in the document, so no new
/// summary arrives to check it again.
pub fn format(app: &mut Writer, command: Command) {
    app.editor.exec(command);
    app.editor.focus();
    if let Some(summary) = &app.summary {
        app.tools.sync(summary, &app.host);
    }
}

pub fn block(app: &mut Writer, index: usize) {
    let kind = match index {
        1..=3 => BlockKind::Heading(index as u8),
        4 => BlockKind::Quote,
        _ => BlockKind::Body,
    };
    debug_assert!(index < BLOCKS.len());
    format(app, Command::SetBlockKind(kind));
}

pub fn family(app: &mut Writer, index: usize) {
    let family = match index {
        1 => Some(app.host.serif_family.clone()),
        2 => Some(app.host.mono_family.clone()),
        _ => None,
    };
    debug_assert!(index < FAMILIES.len());
    let patch = CharStylePatch {
        family: Some(family),
        ..CharStylePatch::default()
    };
    format(app, Command::SetCharStyle(patch));
}

pub fn size(app: &mut Writer, index: usize) {
    let Some(&size) = SIZES.get(index) else {
        return;
    };
    let patch = CharStylePatch {
        size: Some(Dip(size)),
        ..CharStylePatch::default()
    };
    format(app, Command::SetCharStyle(patch));
}

pub fn toggle(app: &mut Writer, mark: Mark) {
    format(
        app,
        match mark {
            Mark::Bold => Command::ToggleBold,
            Mark::Italic => Command::ToggleItalic,
            Mark::Underline => Command::ToggleUnderline,
            Mark::Strike => Command::ToggleStrike,
        },
    );
}

/// The wrap at `index` in [`WRAPS`].
pub fn wrap_at(index: usize) -> Wrap {
    match index {
        1 => Wrap::square(Side::Left),
        2 => Wrap::square(Side::Right),
        3 => Wrap::TopAndBottom { margin: Dip(8.0) },
        _ => Wrap::Inline,
    }
}

/// Sets the wrap of the selected image.
pub fn wrap(app: &mut Writer, index: usize) {
    let Selection::Object(id) = app.editor.selection() else {
        return;
    };
    debug_assert!(index < WRAPS.len());
    format(
        app,
        Command::SetWrap {
            id,
            wrap: wrap_at(index),
        },
    );
}

/// Asks for a link address for the selected text.
pub fn link(app: &mut Writer) {
    if app.dialog_open.get() {
        return;
    }
    app.dialog_open.set(true);
    app.dialogs.link.open();
}

/// The link prompt was dismissed: link the selection to the address, or
/// remove its link when the address is empty.
pub fn link_chosen(app: &mut Writer, action: DialogAction) {
    app.dialog_open.set(false);
    match action {
        DialogAction::Accept(url) => {
            let url = url.trim();
            let link = (!url.is_empty()).then(|| url.to_owned());
            let patch = CharStylePatch {
                link: Some(link),
                ..CharStylePatch::default()
            };
            format(app, Command::SetCharStyle(patch));
        }
        DialogAction::Cancel => app.editor.focus(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_names_the_file_and_marks_changes() {
        assert_eq!(title("Untitled", false), "LazyWriter: Untitled");
        assert_eq!(title("notes.lzw", true), "LazyWriter: *notes.lzw");
    }

    #[test]
    fn the_word_count_reads_naturally() {
        assert_eq!(words_label(0), "0 words");
        assert_eq!(words_label(1), "1 word");
        assert_eq!(words_label(42), "42 words");
    }

    #[test]
    fn each_wrap_choice_maps_to_its_wrap() {
        assert_eq!(wrap_at(0), Wrap::Inline);
        assert_eq!(wrap_at(1), Wrap::square(Side::Left));
        assert_eq!(wrap_at(2), Wrap::square(Side::Right));
        assert!(matches!(wrap_at(3), Wrap::TopAndBottom { .. }));
        assert_eq!(WRAPS.len(), 4);
    }
}
