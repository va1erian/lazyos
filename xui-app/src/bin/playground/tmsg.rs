//! The Topic monitor window's messages, and the requests the main window
//! sends it.
//!
//! The two windows are two [`xui_core::app::App`]s in one process
//! (`Ui::open_window`): the main window keeps a `WindowHandle<TMsg>` and sends
//! a [`Request`] as a [`TMsg::Request`]; the monitor sends [`crate::msg::Msg`]
//! back through the main window's `Proxy`. Nothing is shared but the
//! [`xui_app::devtool::Notice`] and the registry snapshot, which travels as a
//! message.

use xui_playground::listener::Registry;

/// What the main window asks of the monitor. Pure data, so the routing is
/// tested without a window ([`crate::topicshost`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Start a subscription on this filter and show its tab.
    Listen(String),
    /// Put this topic and value in the Publish form (nothing is sent).
    FillPublish { topic: String, payload: String },
    /// Just bring the window up.
    Show,
}

#[derive(Clone)]
pub enum TMsg {
    // ---- from the main window ------------------------------------------
    Request(Request),
    /// The newest registry read (who owns which task slot), for publisher names.
    Registry(Registry),
    /// The shared developer-tool notice changed in the other window.
    NoticeChanged,
    // ---- timers --------------------------------------------------------
    /// Look at the doorbells (a local channel read), only while listening.
    Bell,
    /// Continue draining a subscription whose batch was full.
    Drain,
    Resized,
    ProbeDue,
    // ---- the events ----------------------------------------------------
    SubTab(usize),
    SubRow(usize),
    Pause,
    CloseSub,
    ClearEvents,
    EventFilter(String),
    PinEvent,
    CopyEvent,
    SaveEvents,
    // ---- the Publish form ----------------------------------------------
    PubTopic(String),
    PubPayload(String),
    PubRetained(bool),
    PubGo,
    PubListen,
    // ---- notice, window ------------------------------------------------
    NoticeDismiss,
    NoticeShow,
    Close,
    /// Ctrl+Q here quits the whole tool (the main window closes this one).
    QuitApp,
}
