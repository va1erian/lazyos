//! The main window's messages. The Topic monitor window has its own
//! ([`crate::tmsg::TMsg`]).

use std::path::PathBuf;

use xui_app::devtool::ReviewAnswer;
use xui_core::Dip;

/// What a doc-pane button does with the selected node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Append a call with typed placeholders.
    InsertCall,
    /// Open the generated form (a refused or missing method opens nothing).
    CallForm,
    /// Append a `msg::serve` with one handler per method.
    ServeSkeleton,
    /// Append `msg::connect` to the live service of this interface.
    Connect,
    /// Ask the Topic monitor to start a native subscription on the topic.
    Listen,
    /// Take the topic's retained value without keeping a subscription (a
    /// script, run once; its output is in this window's Output).
    Peek,
    /// Fill the Topic monitor's Publish form for the topic.
    Publish,
    /// Append the typed `on_...` helper for the topic.
    InsertOn,
    /// Put the selected example in the editor.
    LoadExample,
}

/// An edit of the Call form (indices walk the control tree).
#[derive(Clone, Debug)]
pub enum FormEdit {
    Text(Vec<usize>, String),
    Check(Vec<usize>, bool),
    Pick(Vec<usize>, usize),
    AddRow(Vec<usize>),
    RemoveRow(Vec<usize>),
}

#[derive(Clone)]
pub enum Msg {
    // ---- timers and threads -------------------------------------------
    /// The Live tab's refresh (a kernel read).
    Tick,
    /// A run's output or exit is waiting in the runner's channel.
    RunnerWake,
    // ---- explorer ------------------------------------------------------
    ExplorerTab(usize),
    Search(String),
    TreeSelect(usize),
    TreeToggle(usize, bool),
    LiveSelect(usize),
    LiveActivate(usize),
    TopicSelect(usize),
    TopicsRefresh,
    GallerySelect(usize),
    GalleryActivate(usize),
    // ---- doc pane ------------------------------------------------------
    Do(Action),
    OpenLink(String),
    Form(FormEdit),
    FormCall,
    FormInsert,
    FormClose,
    // ---- editor --------------------------------------------------------
    Edited(String),
    Run,
    RunSelection,
    Stop,
    StopAll,
    Reviewed(ReviewAnswer),
    CallConfirmed(bool),
    // ---- output --------------------------------------------------------
    OutputTab(usize),
    OutputRow(u32, usize),
    // ---- the Topic monitor window (`crate::topicswin`) -----------------
    /// View > Topic monitor (Ctrl+T): open the window, or say it is open.
    ShowTopics,
    /// From the monitor: its Publish form asks for this to be published
    /// (a one-line script, run once here, D3).
    PublishRequest {
        topic: String,
        payload: String,
        retained: bool,
    },
    /// From the monitor: how many subscriptions it holds.
    TopicsStatus(usize),
    /// From the monitor: its window closed.
    TopicsGone,
    // ---- the developer-tool notice (`xui_app::devtool::Notice`) --------
    /// The strip's "x".
    NoticeDismiss,
    /// View > Show developer-tool notice.
    NoticeShow,
    /// The other window of the tool changed the shared notice.
    NoticeChanged,
    // ---- files ---------------------------------------------------------
    FileNew,
    FileOpen,
    FileSave,
    FileSaveAs,
    OpenChosen(PathBuf),
    SaveChosen(PathBuf),
    DialogDone,
    // ---- edit, view, help ----------------------------------------------
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    ShowHelp,
    /// Ctrl+J, the Hide/Show editor button and View > Editor and output.
    ToggleBottom,
    /// The Explorer | right column divider was dragged: the Explorer's new width.
    ExplorerMoved(Dip),
    /// The doc pane | bottom row divider was dragged: the doc pane's new height.
    DocSplitMoved(Dip),
    Resized,
    ProbeDue,
    Quit,
    Close,
}

impl Msg {
    /// Messages a key shortcut raises that must wait while a modal card is open.
    pub fn is_shortcut(&self) -> bool {
        matches!(
            self,
            Msg::Run
                | Msg::RunSelection
                | Msg::Stop
                | Msg::FileNew
                | Msg::FileOpen
                | Msg::FileSave
                | Msg::FileSaveAs
                | Msg::ShowHelp
                | Msg::ShowTopics
                | Msg::NoticeShow
                | Msg::ToggleBottom
                | Msg::ExplorerTab(_)
        )
    }
}
