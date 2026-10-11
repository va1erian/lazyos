//! LazySky's portable window (docs/lazysky-plan.md §2): xui code that
//! depends on `lazysky-core` and `xui-core` only. The LazyOS binary and the
//! desktop example (`examples/desktop.rs`) launch it and supply a
//! [`lazysky_core::Platform`]; nothing here may call an OS API.
//!
//! The window never waits on the network or on text shaping (R1): it sends
//! [`lazysky_core::Request`]s to the core, rows are shaped on the layout
//! thread, and a timer drains both within a budget (R5), fast while work is
//! pending and slow when idle (R8).

mod app;
pub mod drain;
pub mod feedview;
mod home;
pub mod layouts;
mod login;
pub mod perf;
pub mod prepare;
mod timeline;

pub use app::LazySkyApp;
pub use feedview::FeedAction;
use lazysky_core::Secret;

/// The window's design size when a compositor lays the app out.
pub const WINDOW: (i32, i32) = (960, 640);

/// What the window tells its host (the binary prints it as serial evidence).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// The timeline now holds this many posts.
    FeedRows(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    /// The timer: drain the core's and the layout thread's answers.
    Tick,
    /// The Sign in button: read the fields.
    SignInClicked,
    /// Sign in with these credentials (the button's message, with the fields
    /// read; tests send it directly).
    Login {
        handle: String,
        password: Secret,
    },
    SignOut,
    LoadMore,
    /// A click in the feed.
    Feed(FeedAction),
    Report(Report),
    Quit,
}
