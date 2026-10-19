//! LazyAgent's portable window (docs/ai-agent-plan.md §2, §9): xui code that
//! depends on `lazyagent-core` and `xui-core` only. The LazyOS binary and the
//! desktop example (`examples/desktop.rs`) launch it and supply a
//! [`lazyagent_core::Platform`]; nothing here may call an OS API.
//!
//! The window never waits on the network, on Markdown or on text shaping
//! (rule 1 of the plan): it sends [`lazyagent_core::Request`]s to the core,
//! rows are parsed and shaped on the layout thread, and a timer drains both
//! within a budget, fast while work is pending and slow when idle.

mod app;
pub mod drain;
pub mod layouts;
pub mod markdown;
pub mod model;
mod pages;
pub mod prepare;
mod rows;
pub mod view;

pub use app::LazyAgentApp;
pub use view::ViewAction;

use xui_core::widget::TaskDialogAction;

/// The window's design size when a compositor lays the app out.
pub const WINDOW: (i32, i32) = (960, 640);

/// What the window tells its host (the binary prints it as serial evidence;
/// none carries message text, a key or a token).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// The window is up and the settings are in.
    Ready,
    /// The consent screen was accepted.
    Consented,
    /// A key was entered (never its value).
    KeySet,
    /// A message was sent: the number of turns in the conversation now.
    Sent(usize),
    /// The first chunk of a reply arrived after this many milliseconds.
    FirstChunk(u64),
    /// A reply is complete: tokens in, tokens out, milliseconds.
    Done {
        prompt: u64,
        completion: u64,
        ms: u64,
    },
    /// A turn failed with this status (`401`, `interrupted`, ...).
    Failed(String),
    /// The conversation on show has this many rows.
    Rows(usize),
    /// A conversation was opened, started or deleted: how many are listed.
    Conversations(usize),
    /// A code block or message was copied.
    Copied,
    /// The view stopped (false) or resumed (true) following the end of the
    /// transcript.
    Following(bool),
    /// A link was asked for (its confirmation is up) or opened.
    LinkAsked(String),
    LinkOpened(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    /// The timer: drain the core's and the layout thread's answers.
    Tick,
    /// The consent screen's button.
    Accept,
    /// The key field's button (or Enter): read the field.
    KeySubmit,
    /// Use this key (the button's message, with the field read; tests send
    /// it directly). Its `Debug` never shows the value.
    KeyEntered(lazyagent_core::Secret),
    /// Send what the composer holds (the button, or Enter).
    Send,
    /// Send this text (the button's message, with the composer read; tests
    /// send it directly).
    SendText(String),
    ComposerText(String),
    Stop,
    Regenerate,
    EditLast,
    NewChat,
    /// A row of the conversation list.
    Select(usize),
    DeleteChat,
    ExportChat,
    ExportPicked(std::path::PathBuf),
    SearchText(String),
    OpenSettings,
    Back,
    SettingsModelPreset(usize),
    SettingsModelText(String),
    SettingsEffort(usize),
    SettingsTheme(usize),
    SettingsSystem(String),
    ForgetKey,
    /// A click in the transcript.
    Transcript(ViewAction),
    /// The link confirmation's answer.
    LinkChoice(TaskDialogAction),
    Report(Report),
    Quit,
}
