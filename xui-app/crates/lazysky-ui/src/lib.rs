//! LazySky's portable window (docs/lazysky-plan.md §2): xui code that
//! depends on `lazysky-core` and `xui-core` only. The LazyOS binary and the
//! desktop example (`examples/desktop.rs`) launch it and supply a
//! [`lazysky_core::Platform`]; nothing here may call an OS API.
//!
//! The window never waits on the network or on text shaping (R1): it sends
//! [`lazysky_core::Request`]s to the core, rows are shaped on the layout
//! thread, and a timer drains both within a budget (R5), fast while work is
//! pending and slow when idle (R8).

mod actions;
mod app;
pub mod drain;
pub mod feedview;
pub mod images;
pub mod layouts;
mod login;
mod pages;
pub mod perf;
pub mod prepare;
mod timeline;
pub mod viewer;

use std::path::PathBuf;

pub use app::LazySkyApp;
pub use feedview::FeedAction;
use lazysky_core::Secret;
use xui_core::widget::MenuId;

/// The window's design size when a compositor lays the app out.
pub const WINDOW: (i32, i32) = (960, 640);

/// What the window tells its host (the binary prints it as serial evidence).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// The timeline now holds this many posts.
    FeedRows(usize),
    /// The detail page (a thread) holds this many posts.
    DetailRows(usize),
    /// The picture cache holds this many pictures.
    Pictures(usize),
    /// The session ended and the window went back to the login page.
    SignedOut,
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
    /// A click in the feed of pane `.0`.
    Feed(u8, FeedAction),
    /// Back out of the page on top (also Alt+Left).
    Back,
    /// Open the composer: a new post (`None`) or a reply to the post with
    /// this URI.
    Compose(Option<String>),
    ComposerText(String),
    ComposerAddPicture,
    ComposerPicture(PathBuf),
    ComposerRemovePicture,
    ComposerPost,
    /// A command of a post's "..." menu.
    PostMenu(MenuId),
    /// A row of the people list (accounts, or notifications).
    PersonSelected(usize),
    /// The search box changed.
    SearchText(String),
    /// Search for what the box holds.
    SearchGo,
    /// Switch a search between accounts and posts.
    SearchToggle,
    OpenNotifications,
    OpenBookmarks,
    /// A profile: `None` is the signed-in user's own.
    OpenProfile(Option<String>),
    /// A profile tab: 0 posts, 1 media, 2 likes.
    ProfileTab(usize),
    OpenSettings,
    /// The theme choice: 0 system, 1 light, 2 dark.
    SettingsTheme(usize),
    SettingsMaxHeight(i32),
    SettingsApplyService,
    SettingsRemember(bool),
    /// The viewer's Save as... (or Ctrl+S).
    ViewerSave,
    /// Zoom the viewer by this many steps.
    ViewerZoom(i32),
    ViewerFit,
    /// Previous (-1) or next (+1) picture of the post.
    ViewerStep(i32),
    /// The Save As dialog accepted this path.
    SavePicked(PathBuf),
    /// A command of the picture's menu.
    ImageMenu(MenuId),
    /// Download every picture of the profile on show.
    BulkStart,
    BulkCancel,
    Report(Report),
    Quit,
}
