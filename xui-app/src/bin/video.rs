//! `xui-video`: the Video Player (`docs/video-player-plan.md`).
//!
//! The window is the portable `xui-videoview` crate over `lazyvideo`; this
//! file supplies the LazyOS platform through `AppContext`: `$HOME` for the
//! Open dialog, a video named on the command line (how `mimed` opens one),
//! and files dropped on the window as `text/uri-list`.
//!
//! Serial evidence: `VIDEO:UP:PASS` after the first frame;
//! `VIDEO:BIND:FAIL:<code>`; the crate's `VIDEO:OPEN|PLAY|PAUSE|SEEK|POS|END|
//! ERROR:*` lines and `VIDEO:QUIT:PASS`.

use xui_app::trace::Channel;
use xui_videoview::{Msg, VideoApp, WINDOW};

fn main() {
    xui_app::app(Channel::Video, "Video Player")
        .size(WINDOW.0, WINDOW.1)
        // Below this the toolbar and the seek bar clip.
        .min_size(360, 280)
        .opens(Msg::OpenChosen)
        .drops(Msg::Dropped)
        .run(|ui, cx| VideoApp::build(ui, cx.host()))
}
