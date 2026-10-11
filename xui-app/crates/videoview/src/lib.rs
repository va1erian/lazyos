//! The Video Player (`os.lazy.video`, docs/video-player-plan.md): a picture
//! view over `lazyvideo`'s player thread, a transport toolbar, a seek bar and
//! a status bar, in one xui window.
//!
//! [`VideoApp`] is the window; the LazyOS binary (`xui-app/src/bin/video.rs`)
//! hands it a [`Host`] and a file from the command line. Evidence lines:
//! `VIDEO:OPEN:PASS:<path>:<w>x<h>:<fps>:<frames>:<codec>`,
//! `VIDEO:OPEN:FAIL:<path>:<reason>`, `VIDEO:PLAY` / `VIDEO:PAUSE`,
//! `VIDEO:SEEK:<ms>`, `VIDEO:POS:<ms>:shown=<n>:dropped=<n>:fps=<n>` once a
//! second while playing, `VIDEO:END:PASS:shown=<n>:dropped=<n>`,
//! `VIDEO:ERROR:<reason>` and `VIDEO:QUIT:PASS`.

#![forbid(unsafe_code)]

mod app;
mod host;
mod ui;
mod view;

pub use app::{shortcut, Msg, VideoApp, WINDOW};
pub use host::{Host, LogFn};
