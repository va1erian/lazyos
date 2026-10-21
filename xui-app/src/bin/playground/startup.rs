//! Building the window: the layout, the dialogs, the timers and the first
//! readings.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use xui_app::backend::LazyOSBackend;
use xui_app::devtool::{Notice, Provenance, ReviewGate};
use xui_app::trace::{self, Channel};
use xui_core::app::Ui;
use xui_core::arrange::Handle;
use xui_core::backend::{Event, Result, WidgetId};
use xui_core::widget::{FileDialog, StdFileSystem};
use xui_core::{Key, Modifiers};
use xui_playground::arrival::Known;
use xui_playground::docs::Block;
use xui_playground::runner::RunTable;

use crate::app::{Playground, Sel, Timers};
use crate::explorer::Explorer;
use crate::lifecycle::TICK_MS;
use crate::msg::Msg;
use crate::panesapp::PaneState;
use crate::proc::ProcRunner;
use crate::topicshost::TopicsHost;
use crate::ui::Widgets;

/// What the editor holds when the window opens. It is the app's own text
/// (nothing asks before running it), and it does not run until Run is pressed.
const WELCOME: &str = "// Messenger Playground. Write Rhai here and press Run (F5).\n\
// Pick an interface on the left and use Insert call, or open the Gallery.\n\
for name in msg::services() {\n    print(name);\n}\n";

pub fn build(ui: &mut Ui<Msg>, backend: &Rc<LazyOSBackend>) -> Result<Playground> {
    // Below this the panes clip.
    backend.set_size_hints(900, 560, 0, 0);
    backend.on_first_frame(|| trace::pass!(Channel::Playground, "UP"));
    if let Some(theme) = backend.desktop_theme() {
        ui.set_theme(theme);
    }
    // One notice for the tool's windows: the strip's state, remembered per user.
    let notice = Notice::for_tool(crate::notice::SYSTEM_NAME);
    crate::notice::announce_stored(&notice);
    // The doc pane paints only what is being repainted: where that is, in its own pixels.
    {
        let backend = Rc::clone(backend);
        crate::docbody::set_damage_source(Rc::new(move |id| {
            let damage = backend.paint_damage()?;
            let node = backend.window_bounds(id)?;
            Some(damage.offset(-node.left, -node.top))
        }));
    }
    let w = Widgets::default();
    let panes = PaneState::stored();
    ui.root(w.layout(&notice, &panes.saved))?;
    let editor = Rc::clone(&w.editor.get().0);

    let open_dialog = FileDialog::open_file(ui, "Open script")?
        .file_system(Rc::new(StdFileSystem))
        .initial_dir(crate::files::scripts_dir())
        .filter("Rhai scripts", &["rhai"])
        .filter("All files", &[])
        .require_existing(true)
        .on_accept(|path| Some(Msg::OpenChosen(path)))
        .on_cancel(|| Some(Msg::DialogDone));
    let save_dialog = FileDialog::save_file(ui, "Save script")?
        .file_system(Rc::new(StdFileSystem))
        .initial_dir(crate::files::scripts_dir())
        .suggested_name("script.rhai")
        .filter("Rhai scripts", &["rhai"])
        .on_accept(|path| Some(Msg::SaveChosen(path)))
        .on_cancel(|| Some(Msg::DialogDone));

    ui.on_key(|key, modifiers| shortcut(key, modifiers));
    ui.on_close(|| Some(Msg::Close));
    ui.register_events(WidgetId::NONE, |event| {
        matches!(event, Event::Resize { .. }).then_some(Msg::Resized)
    });
    let timers = Timers::default();
    timers.tick.set(Some(ui.set_timer(TICK_MS)));
    {
        let timers = timers.clone();
        ui.on_timer(move |id| {
            let is = |cell: &Rc<Cell<Option<xui_core::backend::TimerId>>>| cell.get() == Some(id);
            if is(&timers.probe) {
                Some(Msg::ProbeDue)
            } else if is(&timers.tick) {
                Some(Msg::Tick)
            } else {
                None
            }
        });
    }

    let mut known = Known::default();
    known.remember(WELCOME);
    let mut app = Playground {
        w,
        backend: Rc::clone(backend),
        panes,
        title: xui_app::devtool::title("Messenger Playground"),
        started: Instant::now(),
        ex: Explorer::new(),
        tab: 0,
        sel: Sel::Nothing,
        selected_service: None,
        form: None,
        form_first: Handle::new(),
        editor,
        text: WELCOME.to_string(),
        gate: ReviewGate::new(Provenance::Typed, WELCOME),
        known,
        file: None,
        dirty: false,
        pending: None,
        review: None,
        confirm_card: None,
        pending_call: None,
        file_label_override: None,
        open_dialog,
        save_dialog,
        runs: RunTable::new(ProcRunner::new(ui.proxy())),
        pages: Vec::new(),
        welcome_page: true,
        calls: Vec::new(),
        topics: TopicsHost::default(),
        notice,
        broker: xui_app::fabric::Broker::new(),
        topics_read: None,
        timers,
        last_live: Vec::new(),
        last_live_ms: 0,
        probe_due: Cell::new(true),
    };
    app.editor.set_text(WELCOME);
    app.editor.goto(0, 0);
    app.tick(ui);
    app.refill_tree();
    app.refill_live();
    app.refill_topics();
    app.refill_gallery();
    app.show_blocks(&[
        Block::Title("Messenger Playground".to_string()),
        Block::Paragraph(
            "Pick an interface, a method or a topic on the left to read its documentation.              The buttons above write Rhai into the editor or run it once; nothing runs until              you press Run (F5)."
                .to_string(),
        ),
        Block::Paragraph(
            "Listen opens the Topic monitor window and shows a topic's events decoded with the type the interface declares. The              Gallery tab has examples to load. Help (F1) says what this tool can do."
                .to_string(),
        ),
    ]);
    app.enable_buttons();
    app.update_file_label();
    app.update_status();
    app.note("Ready. Nothing runs until you press Run.");
    app.fit_panes(ui);
    app.focus_editor();
    Ok(app)
}

/// The keys the window answers wherever the focus is.
pub fn shortcut(key: Key, modifiers: Modifiers) -> Option<Msg> {
    let command = modifiers.ctrl || modifiers.win;
    if key == Key::F5 {
        return Some(if modifiers.shift { Msg::Stop } else { Msg::Run });
    }
    if key == Key::F1 {
        return Some(Msg::ShowHelp);
    }
    if command {
        return match key {
            Key::RETURN => Some(Msg::RunSelection),
            Key::DIGIT1 => Some(Msg::ExplorerTab(0)),
            Key::DIGIT2 => Some(Msg::ExplorerTab(1)),
            Key::DIGIT3 => Some(Msg::ExplorerTab(2)),
            Key::DIGIT4 => Some(Msg::ExplorerTab(3)),
            Key::T => Some(Msg::ShowTopics),
            Key::J => Some(Msg::ToggleBottom),
            Key::D if modifiers.shift => Some(Msg::NoticeShow),
            Key::N => Some(Msg::FileNew),
            Key::O => Some(Msg::FileOpen),
            Key::S if modifiers.shift => Some(Msg::FileSaveAs),
            Key::S => Some(Msg::FileSave),
            Key::Q => Some(Msg::Quit),
            _ => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl() -> Modifiers {
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        }
    }

    #[test]
    fn run_and_stop_are_f5_and_shift_f5_and_nothing_runs_on_other_keys() {
        assert!(matches!(shortcut(Key::F5, Modifiers::default()), Some(Msg::Run)));
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert!(matches!(shortcut(Key::F5, shift), Some(Msg::Stop)));
        assert!(shortcut(Key::RETURN, Modifiers::default()).is_none());
        assert!(matches!(shortcut(Key::RETURN, ctrl()), Some(Msg::RunSelection)));
        assert!(shortcut(Key::Q, Modifiers::default()).is_none(), "typing q is typing");
        assert!(matches!(shortcut(Key::Q, ctrl()), Some(Msg::Quit)));
    }

    #[test]
    fn the_topic_monitor_and_the_notice_have_shortcuts() {
        assert!(matches!(shortcut(Key::T, ctrl()), Some(Msg::ShowTopics)));
        assert!(shortcut(Key::T, Modifiers::default()).is_none(), "typing t is typing");
        let shift = Modifiers {
            shift: true,
            ..ctrl()
        };
        assert!(matches!(shortcut(Key::D, shift), Some(Msg::NoticeShow)));
        assert!(shortcut(Key::D, ctrl()).is_none());
    }

    #[test]
    fn ctrl_j_toggles_the_bottom_row_and_a_plain_j_is_typing() {
        assert!(matches!(shortcut(Key::J, ctrl()), Some(Msg::ToggleBottom)));
        assert!(shortcut(Key::J, Modifiers::default()).is_none());
    }
}
