//! The Playground's state and message dispatch. The behaviour of each area
//! lives in its own module as another `impl Playground` block: [`crate::selection`]
//! (selection and the doc buttons), [`crate::runs`] (the editor's Run and the
//! Output tabs), [`crate::topicshost`] (the Topic monitor window, whose own
//! state is [`crate::topicswin::TopicsTool`]), [`crate::files`].

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use xui_app::backend::LazyOSBackend;
use xui_app::devtool::{Notice, ReviewGate};
use xui_code_editor::Editor;
use xui_core::app::{App, Ui};
use xui_core::arrange::Handle;
use xui_core::backend::TimerId;
use xui_core::widget::{Edit, FileDialog, TaskDialog};
use xui_playground::arrival::Known;
use xui_playground::catalog::NodeId;
use xui_playground::form::Form;
use xui_playground::runner::{RunId, RunTable};

use crate::explorer::Explorer;
use crate::msg::Msg;
use crate::panesapp::PaneState;
use crate::proc::ProcRunner;
use crate::topicshost::TopicsHost;
use crate::ui::Widgets;

/// What the doc pane is showing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sel {
    Nothing,
    Node(NodeId),
    /// A live topic nothing declares.
    LiveTopic(String),
    Example(&'static str),
    Help,
}

/// An open Call form.
pub struct OpenForm {
    pub interface: &'static str,
    pub method: &'static str,
    pub form: Form,
    pub error: Option<String>,
}

/// One Output tab.
pub struct RunPage {
    pub id: RunId,
    pub list: Handle<xui_core::widget::ListView<Msg>>,
    /// What the list last showed: line count and state, to skip a repaint.
    pub signature: (u64, u8),
}

/// One Topic monitor tab.
pub struct SubPage {
    pub list: Handle<xui_core::widget::ListView<crate::tmsg::TMsg>>,
    /// The id of the event the details show.
    pub selected: Option<u64>,
    /// The newest event id already traced.
    pub traced: u64,
    /// How many `EVENT` lines this tab may trace (it can be a firehose).
    pub budget: xui_app::trace::Budget,
}

/// The Publish form's fields.
#[derive(Default)]
pub struct PubForm {
    pub topic: String,
    pub payload: String,
    pub retained: bool,
}

/// The timers the window owns; each exists only while it has work.
#[derive(Clone, Default)]
pub struct Timers {
    pub tick: Rc<Cell<Option<TimerId>>>,
    pub probe: Rc<Cell<Option<TimerId>>>,
}

/// Why a run is waiting on the review card.
#[derive(Clone)]
pub struct PendingRun {
    pub script: String,
    pub first_line: usize,
    pub title: String,
}

pub struct Playground {
    pub w: Widgets,
    /// Where nodes are in the window (the UI probe prints window coordinates).
    pub backend: Rc<LazyOSBackend>,
    /// The dividers and the bottom row's switch (`panesapp.rs`).
    pub panes: PaneState,
    pub title: String,
    pub started: Instant,
    pub ex: Explorer,
    pub tab: usize,
    pub sel: Sel,
    pub selected_service: Option<String>,
    pub form: Option<OpenForm>,
    pub form_first: Handle<Edit<Msg>>,
    // editor
    pub editor: Rc<Editor<Msg>>,
    pub text: String,
    pub gate: ReviewGate,
    /// Texts the gate vouched for: Undo and Redo go back to them freely.
    pub known: Known,
    pub file: Option<PathBuf>,
    pub dirty: bool,
    pub pending: Option<PendingRun>,
    pub review: Option<TaskDialog<Msg>>,
    pub confirm_card: Option<TaskDialog<Msg>>,
    pub pending_call: Option<(String, String, String)>,
    pub file_label_override: Option<String>,
    pub open_dialog: FileDialog<Msg>,
    pub save_dialog: FileDialog<Msg>,
    // runs
    pub runs: RunTable<ProcRunner>,
    pub pages: Vec<RunPage>,
    pub welcome_page: bool,
    /// Runs started by a Call form: the `interface.method` the marker names.
    pub calls: Vec<(RunId, String)>,
    // the Topic monitor window and the shared developer-tool notice
    pub topics: TopicsHost,
    pub notice: Rc<Notice>,
    pub broker: xui_app::fabric::Broker,
    pub topics_read: Option<Instant>,
    pub timers: Timers,
    pub last_live: Vec<Vec<String>>,
    pub last_live_ms: u64,
    /// The layout changed: print the probe lines once it settles.
    pub probe_due: Cell<bool>,
}

impl Playground {
    /// Whether a dialog (a file picker, the review or the confirm card) is up.
    pub fn modal_open(&self) -> bool {
        self.open_dialog.is_open()
            || self.save_dialog.is_open()
            || self.review.as_ref().is_some_and(|d| d.is_open())
            || self.confirm_card.as_ref().is_some_and(|d| d.is_open())
    }

    /// Milliseconds since the window started (the models' clock).
    pub fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

}

impl App for Playground {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        // A modal card owns the keyboard: the shortcuts (F5, Ctrl+O, ...) do nothing behind it.
        if self.modal_open() && msg.is_shortcut() {
            return;
        }
        match msg {
            Msg::Tick => self.tick(ui),
            Msg::RunnerWake => self.runner_wake(),
            Msg::ExplorerTab(i) => self.explorer_tab(ui, i),
            Msg::Search(text) => self.search(&text),
            Msg::TreeSelect(row) => self.tree_select(ui, row),
            Msg::TreeToggle(row, open) => self.ex.toggled(row, open),
            Msg::LiveSelect(row) => self.live_select(ui, row),
            Msg::LiveActivate(row) => self.live_activate(ui, row),
            Msg::TopicSelect(row) => self.topic_select(ui, row),
            Msg::TopicsRefresh => {
                self.tick(ui);
                self.refresh_topics(true);
            }
            Msg::GallerySelect(row) => self.gallery_select(ui, row),
            Msg::GalleryActivate(row) => {
                self.gallery_select(ui, row);
                self.act(ui, crate::msg::Action::LoadExample);
            }
            Msg::Do(action) => self.act(ui, action),
            Msg::OpenLink(path) => self.open_link(&path),
            Msg::Form(edit) => self.form_edit(ui, &edit),
            Msg::FormCall => self.form_call(ui),
            Msg::FormInsert => self.form_insert(),
            Msg::FormClose => self.form_close(ui),
            Msg::Edited(text) => self.edited(text),
            Msg::Run => self.run_buffer(ui, false),
            Msg::RunSelection => self.run_buffer(ui, true),
            Msg::Stop => self.stop_selected(),
            Msg::StopAll => self.runs.stop_all(),
            Msg::Reviewed(answer) => self.reviewed(ui, answer),
            Msg::CallConfirmed(yes) => self.call_confirmed(yes),
            Msg::Resized => {
                self.probe_soon();
                self.fit_panes(ui);
            }
            Msg::ToggleBottom => self.toggle_bottom(ui),
            Msg::ExplorerMoved(width) => self.explorer_moved(width),
            Msg::DocSplitMoved(height) => self.doc_split_moved(ui, height),
            Msg::ProbeDue => self.probe_now(ui),
            Msg::OutputTab(i) => self.output_tab(i),
            Msg::OutputRow(id, row) => self.output_row(id, row),
            Msg::ShowTopics => self.show_topics(ui),
            Msg::PublishRequest { topic, payload, retained } => {
                self.publish_request(&topic, &payload, retained)
            }
            Msg::TopicsStatus(n) => self.topics.listening = n,
            Msg::TopicsGone => self.topics.listening = 0,
            Msg::NoticeDismiss => self.notice_dismiss(),
            Msg::NoticeShow => self.notice_show(),
            Msg::NoticeChanged => self.notice_changed(),
            Msg::FileNew => self.file_new(),
            Msg::FileOpen => self.file_open(),
            Msg::FileSave => self.file_save(),
            Msg::FileSaveAs => self.file_save_as(),
            Msg::OpenChosen(path) => self.open_chosen(path),
            Msg::SaveChosen(path) => self.save_chosen(path),
            Msg::DialogDone => self.focus_editor(),
            Msg::Undo => {
                self.editor.undo();
            }
            Msg::Redo => {
                self.editor.redo();
            }
            Msg::Cut => {
                self.editor.cut();
            }
            Msg::Copy => {
                self.editor.copy();
            }
            Msg::Paste => {
                self.editor.paste();
            }
            Msg::SelectAll => self.editor.select_all(),
            Msg::ShowHelp => self.show_help(ui),
            Msg::Quit | Msg::Close => self.quit(ui),
        }
        self.after(ui);
    }
}
